//! The keyword index behind `search`, `recent` and `read {link}` (spec
//! §9.3): one entry per Markdown note, kept in memory by the host and fed
//! one file event at a time.

use chrono::NaiveDate;
use std::collections::BTreeMap;

use crate::rpc::{RecentNote, SearchHit, SearchQuery};

const SNIPPET_CHARS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub path: String,
    pub title: String,
    pub modified: NaiveDate,
    pub body: String,
    /// The title's and body's words, lowercased once at upsert so a query
    /// never re-tokenises a note.
    title_words: Vec<String>,
    body_words: Vec<String>,
}

/// The first `# ` heading of a note, else its file stem.
pub fn note_title(path: &str, body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find_map(|line| {
            line.strip_prefix("# ")
                .map(str::trim)
                .filter(|title| !title.is_empty())
        })
        .map(str::to_string)
        .unwrap_or_else(|| stem(path).to_string())
}

fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".md").unwrap_or(name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkResolution {
    Found(String),
    Ambiguous(Vec<String>),
    Missing,
}

#[derive(Debug, Default, Clone)]
pub struct SearchIndex {
    entries: BTreeMap<String, IndexEntry>,
}

impl SearchIndex {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn upsert(&mut self, path: &str, modified: NaiveDate, body: String) {
        let title = note_title(path, &body);
        let title_words = word_list(&title);
        let body_words = word_list(&body);
        self.entries.insert(
            path.to_string(),
            IndexEntry {
                path: path.to_string(),
                title,
                modified,
                body,
                title_words,
                body_words,
            },
        );
    }

    pub fn remove(&mut self, path: &str) {
        self.entries.remove(path);
    }

    pub fn get(&self, path: &str) -> Option<&IndexEntry> {
        self.entries.get(path)
    }

    /// Whether a path is one the index would hold: a `.md` file outside
    /// any dot-directory.
    pub fn indexes(path: &str) -> bool {
        path.ends_with(".md") && crate::rules::validate_vault_path(path).is_ok()
    }

    /// Keyword search (§9.3): every term must match (whole word,
    /// case-insensitive, `prefix*` allowed), title hits count three times a
    /// body hit, and this month's notes outrank last year's at equal text.
    pub fn search(&self, query: &SearchQuery, today: NaiveDate) -> Vec<SearchHit> {
        let terms: Vec<Term> = query
            .query
            .split_whitespace()
            .map(Term::parse)
            .filter(|term| !term.text.is_empty())
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let folder = query
            .folder
            .as_deref()
            .map(|folder| folder.trim_matches('/').to_string())
            .filter(|folder| !folder.is_empty());
        let mut hits: Vec<(f64, SearchHit)> = Vec::new();
        for entry in self.entries.values() {
            if let Some(since) = query.since
                && entry.modified < since
            {
                continue;
            }
            if let Some(folder) = &folder
                && !in_folder(&entry.path, folder)
            {
                continue;
            }
            let (title_words, body_words) = (&entry.title_words, &entry.body_words);
            let mut score = 0.0;
            let mut all_match = true;
            for term in &terms {
                let in_title = title_words.iter().filter(|w| term.matches(w)).count();
                let in_body = body_words.iter().filter(|w| term.matches(w)).count();
                if in_title + in_body == 0 {
                    all_match = false;
                    break;
                }
                score += 3.0 * in_title as f64 + in_body as f64;
            }
            if !all_match {
                continue;
            }
            let age_days = (today - entry.modified).num_days().max(0) as f64;
            score *= 1.0 + 1.0 / (1.0 + age_days / 30.0);
            hits.push((
                score,
                SearchHit {
                    path: entry.path.clone(),
                    title: entry.title.clone(),
                    modified: entry.modified,
                    snippet: snippet(&entry.body, &terms[0]),
                },
            ));
        }
        hits.sort_by(|(a, hit_a), (b, hit_b)| {
            b.partial_cmp(a)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| hit_b.modified.cmp(&hit_a.modified))
                .then_with(|| hit_a.path.cmp(&hit_b.path))
        });
        hits.into_iter()
            .take(query.limit.max(1))
            .map(|(_, hit)| hit)
            .collect()
    }

    /// Notes modified within `days` of `today`, newest first.
    pub fn recent(&self, days: u32, folder: Option<&str>, today: NaiveDate) -> Vec<RecentNote> {
        let folder = folder
            .map(|folder| folder.trim_matches('/').to_string())
            .filter(|folder| !folder.is_empty());
        let oldest = today - chrono::Duration::days(i64::from(days));
        let mut notes: Vec<RecentNote> = self
            .entries
            .values()
            .filter(|entry| entry.modified >= oldest)
            .filter(|entry| {
                folder
                    .as_ref()
                    .is_none_or(|folder| in_folder(&entry.path, folder))
            })
            .map(|entry| RecentNote {
                path: entry.path.clone(),
                title: entry.title.clone(),
                modified: entry.modified,
            })
            .collect();
        notes.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.path.cmp(&b.path))
        });
        notes
    }

    /// A `[[wikilink]]` by title, then by file name, case-insensitive (§8).
    pub fn resolve_link(&self, link: &str) -> LinkResolution {
        let wanted = link
            .trim()
            .trim_start_matches("[[")
            .trim_end_matches("]]")
            .split('|')
            .next()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        if wanted.is_empty() {
            return LinkResolution::Missing;
        }
        let by_title: Vec<String> = self
            .entries
            .values()
            .filter(|entry| entry.title.to_lowercase() == wanted)
            .map(|entry| entry.path.clone())
            .collect();
        match by_title.len() {
            1 => return LinkResolution::Found(by_title[0].clone()),
            n if n > 1 => return LinkResolution::Ambiguous(by_title),
            _ => {}
        }
        let by_name: Vec<String> = self
            .entries
            .keys()
            .filter(|path| stem(path).to_lowercase() == wanted)
            .cloned()
            .collect();
        match by_name.len() {
            0 => LinkResolution::Missing,
            1 => LinkResolution::Found(by_name[0].clone()),
            _ => LinkResolution::Ambiguous(by_name),
        }
    }
}

fn in_folder(path: &str, folder: &str) -> bool {
    path.strip_prefix(folder)
        .is_some_and(|rest| rest.starts_with('/'))
}

struct Term {
    text: String,
    prefix: bool,
}

impl Term {
    fn parse(raw: &str) -> Self {
        let lower = raw.to_lowercase();
        match lower.strip_suffix('*') {
            Some(text) => Self {
                text: text.to_string(),
                prefix: true,
            },
            None => Self {
                text: lower,
                prefix: false,
            },
        }
    }

    fn matches(&self, word: &str) -> bool {
        if self.prefix {
            word.starts_with(&self.text)
        } else {
            word == self.text
        }
    }
}

fn word_list(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect()
}

/// Up to 240 characters around the first line that holds the term, on
/// line boundaries.
fn snippet(body: &str, term: &Term) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let hit = lines
        .iter()
        .position(|line| word_list(line).iter().any(|word| term.matches(word)))
        .unwrap_or(0);
    let mut collected: Vec<&str> = Vec::new();
    let mut chars = 0usize;
    for line in lines.iter().skip(hit) {
        let line = line.trim();
        if line.is_empty() {
            if collected.is_empty() {
                continue;
            }
            break;
        }
        let length = line.chars().count();
        if chars + length > SNIPPET_CHARS {
            if collected.is_empty() {
                collected.push(line);
            }
            break;
        }
        collected.push(line);
        chars += length + 1;
    }
    let text = collected.join(" ");
    if text.chars().count() > SNIPPET_CHARS {
        let cut: String = text.chars().take(SNIPPET_CHARS - 1).collect();
        format!("{cut}…")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn index() -> SearchIndex {
        let mut index = SearchIndex::default();
        index.upsert(
            "daily/2026-11-03.md",
            day(2026, 11, 3),
            "# Tuesday\n\n- Met Ana about the newsletter\n- Gym\n".into(),
        );
        index.upsert(
            "daily/2025-11-03.md",
            day(2025, 11, 3),
            "# A year ago\n\n- Met Ana about the newsletter\n".into(),
        );
        index.upsert(
            "memory/people/ana.md",
            day(2026, 10, 1),
            "# Ana\n\n- Manager. Prefers written updates.\n- Last mentioned: 2026-10-01\n".into(),
        );
        index.upsert(
            "projects/newsletter.md",
            day(2026, 9, 1),
            "Plain file without a heading.\nThe newsletter fix carried over.\n".into(),
        );
        index
    }

    fn query(text: &str) -> SearchQuery {
        SearchQuery {
            query: text.into(),
            limit: 10,
            since: None,
            folder: None,
        }
    }

    #[test]
    fn titles_come_from_the_first_heading_else_the_stem() {
        assert_eq!(note_title("x/y.md", "intro\n# Real title\n"), "Real title");
        assert_eq!(
            note_title("daily/2026-11-03.md", "no heading"),
            "2026-11-03"
        );
    }

    #[test]
    fn title_hits_beat_body_hits_and_recency_breaks_ties() {
        let hits = index().search(&query("ana"), day(2026, 11, 4));
        let paths: Vec<&str> = hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "memory/people/ana.md",
                "daily/2026-11-03.md",
                "daily/2025-11-03.md"
            ]
        );
        assert_eq!(hits[0].title, "Ana");
        assert_eq!(hits[1].snippet, "- Met Ana about the newsletter - Gym");
    }

    #[test]
    fn every_term_must_match_and_prefixes_work() {
        let index = index();
        let today = day(2026, 11, 4);
        assert!(index.search(&query("ana gym"), today).len() == 1);
        assert!(index.search(&query("ana nowhere"), today).is_empty());
        let hits = index.search(&query("news*"), today);
        assert_eq!(hits.len(), 3);
        assert!(index.search(&query("news"), today).is_empty());
        assert!(index.search(&query("   "), today).is_empty());
    }

    #[test]
    fn since_and_folder_filter_before_scoring() {
        let index = index();
        let today = day(2026, 11, 4);
        let mut since = query("ana");
        since.since = Some(day(2026, 1, 1));
        let paths: Vec<String> = index
            .search(&since, today)
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        assert_eq!(paths, ["memory/people/ana.md", "daily/2026-11-03.md"]);
        let mut folder = query("ana");
        folder.folder = Some("daily/".into());
        let paths: Vec<String> = index
            .search(&folder, today)
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        assert_eq!(paths, ["daily/2026-11-03.md", "daily/2025-11-03.md"]);
        let mut limited = query("ana");
        limited.limit = 1;
        assert_eq!(index.search(&limited, today).len(), 1);
    }

    #[test]
    fn recent_lists_the_window_newest_first() {
        let index = index();
        let notes = index.recent(40, None, day(2026, 11, 4));
        let paths: Vec<&str> = notes.iter().map(|note| note.path.as_str()).collect();
        assert_eq!(paths, ["daily/2026-11-03.md", "memory/people/ana.md"]);
        assert_eq!(notes[0].title, "Tuesday");
        let only_daily = index.recent(400, Some("daily"), day(2026, 11, 4));
        assert_eq!(only_daily.len(), 2);
    }

    #[test]
    fn links_resolve_by_title_then_by_name() {
        let mut index = index();
        assert_eq!(
            index.resolve_link("[[ana]]"),
            LinkResolution::Found("memory/people/ana.md".into())
        );
        assert_eq!(
            index.resolve_link("newsletter"),
            LinkResolution::Found("projects/newsletter.md".into())
        );
        assert_eq!(index.resolve_link("[[Nobody]]"), LinkResolution::Missing);
        index.upsert("people/ana.md", day(2026, 1, 1), "# Ana\n".into());
        assert!(matches!(
            index.resolve_link("Ana|alias"),
            LinkResolution::Ambiguous(paths) if paths.len() == 2
        ));
        index.remove("people/ana.md");
        assert!(matches!(
            index.resolve_link("Ana"),
            LinkResolution::Found(_)
        ));
    }

    #[test]
    fn only_markdown_outside_dot_folders_is_indexed() {
        assert!(SearchIndex::indexes("daily/x.md"));
        assert!(!SearchIndex::indexes(".thock/x.md"));
        assert!(!SearchIndex::indexes("images/x.png"));
    }

    #[test]
    fn snippets_stay_within_the_cap() {
        let long = "word ".repeat(100);
        let body = format!("# T\n\n{long}\n");
        let text = snippet(&body, &Term::parse("word"));
        assert!(text.chars().count() <= SNIPPET_CHARS);
        assert!(text.ends_with('…'));
    }
}
