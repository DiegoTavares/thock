//! `[[collection]]` rows for the Routines panel: the notes directly inside a
//! vault folder, titled and optionally grouped by a field they carry. Parsing
//! and grouping are pure; `load_collection` is the only I/O, through the
//! project `Fs`.

use crate::routines::RoutineCollection;
use fs::Fs;
use std::path::Path;
use std::sync::Arc;

/// One note in a collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionEntry {
    /// `<collection id>/<note stem>` — the `thock::OpenLink` link id.
    pub link_id: String,
    pub title: String,
    /// Vault-relative path of the note.
    pub rel_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionGroup {
    pub label: String,
    pub entries: Vec<CollectionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectionItems {
    Flat(Vec<CollectionEntry>),
    Grouped(Vec<CollectionGroup>),
}

/// A collection ready to render, in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionView {
    pub id: String,
    pub name: String,
    pub items: CollectionItems,
}

impl CollectionView {
    pub fn is_empty(&self) -> bool {
        match &self.items {
            CollectionItems::Flat(entries) => entries.is_empty(),
            CollectionItems::Grouped(groups) => groups.is_empty(),
        }
    }

    pub fn entries(&self) -> Box<dyn Iterator<Item = &CollectionEntry> + '_> {
        match &self.items {
            CollectionItems::Flat(entries) => Box::new(entries.iter()),
            CollectionItems::Grouped(groups) => {
                Box::new(groups.iter().flat_map(|group| group.entries.iter()))
            }
        }
    }
}

/// A note read from the collection's folder.
pub struct CollectionNote {
    pub stem: String,
    pub contents: String,
}

/// Builds the view: titles from each note's first `# ` heading (the file
/// stem when there is none), sorted case-insensitively, and — with
/// `group_by` — bucketed by that field, notes without it last.
pub fn build_collection_view(
    collection: &RoutineCollection,
    notes: Vec<CollectionNote>,
) -> CollectionView {
    let mut keyed: Vec<(Option<String>, CollectionEntry)> = notes
        .into_iter()
        .map(|note| {
            let field = collection
                .group_by
                .as_deref()
                .and_then(|field| note_field(&note.contents, field));
            let title = note_title(&note.contents).unwrap_or_else(|| note.stem.clone());
            let entry = CollectionEntry {
                link_id: format!("{}/{}", collection.id, note.stem),
                title,
                rel_path: format!("{}/{}.md", collection.path, note.stem),
            };
            (field, entry)
        })
        .collect();
    keyed.sort_by_cached_key(|(_, entry)| sort_key(&entry.title));

    let items = match &collection.group_by {
        None => CollectionItems::Flat(keyed.into_iter().map(|(_, entry)| entry).collect()),
        Some(field) => {
            let mut groups: Vec<CollectionGroup> = Vec::new();
            let mut ungrouped = Vec::new();
            for (label, entry) in keyed {
                let Some(label) = label else {
                    ungrouped.push(entry);
                    continue;
                };
                match groups
                    .iter_mut()
                    .find(|group| sort_key(&group.label) == sort_key(&label))
                {
                    Some(group) => group.entries.push(entry),
                    None => groups.push(CollectionGroup {
                        label,
                        entries: vec![entry],
                    }),
                }
            }
            groups.sort_by_cached_key(|group| sort_key(&group.label));
            if !ungrouped.is_empty() {
                groups.push(CollectionGroup {
                    label: format!("No {field}"),
                    entries: ungrouped,
                });
            }
            CollectionItems::Grouped(groups)
        }
    };
    CollectionView {
        id: collection.id.clone(),
        name: collection.name.clone(),
        items,
    }
}

fn sort_key(text: &str) -> String {
    text.to_lowercase()
}

fn split_frontmatter(contents: &str) -> (Option<&str>, &str) {
    let Some(rest) = contents.strip_prefix("---\n") else {
        return (None, contents);
    };
    match rest.find("\n---") {
        Some(end) => {
            let body = &rest[end + 4..];
            (Some(&rest[..end]), body.strip_prefix('\n').unwrap_or(body))
        }
        None => (None, contents),
    }
}

fn note_title(contents: &str) -> Option<String> {
    let (_, body) = split_frontmatter(contents);
    body.lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
}

/// A field's value from frontmatter (`field: value`), else from the first
/// `- Field: value` list line in the body — the shape Readwise-style metadata
/// sections use. Matching is case-insensitive; a `[[wikilink]]` value yields
/// its target.
fn note_field(contents: &str, field: &str) -> Option<String> {
    fn value_after<'a>(line: &'a str, field: &str) -> Option<&'a str> {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(field).then_some(value)
    }

    let (frontmatter, body) = split_frontmatter(contents);
    let raw = frontmatter
        .and_then(|frontmatter| {
            frontmatter
                .lines()
                .filter(|line| !line.starts_with([' ', '\t']))
                .find_map(|line| value_after(line, field))
        })
        .or_else(|| {
            body.lines().find_map(|line| {
                let item = line
                    .strip_prefix("- ")
                    .or_else(|| line.strip_prefix("* "))?;
                value_after(item, field)
            })
        })?;
    let value = raw.trim().trim_matches(['"', '\'']).trim();
    let value = match value
        .strip_prefix("[[")
        .and_then(|inner| inner.strip_suffix("]]"))
    {
        Some(link) => link.split('|').next().unwrap_or(link).trim(),
        None => value,
    };
    (!value.is_empty()).then(|| value.to_string())
}

/// Reads every `*.md` directly inside the collection's folder. A missing
/// folder or an unreadable note is an empty state, not an error.
pub async fn load_collection(
    fs: &Arc<dyn Fs>,
    vault_root: &Path,
    collection: &RoutineCollection,
) -> CollectionView {
    use futures::StreamExt as _;
    let mut notes = Vec::new();
    if let Ok(mut entries) = fs.read_dir(&vault_root.join(&collection.path)).await {
        while let Some(entry) = entries.next().await {
            let Ok(path) = entry else { continue };
            if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if stem.starts_with('.') {
                continue;
            }
            let Ok(contents) = fs.load(&path).await else {
                continue;
            };
            notes.push(CollectionNote {
                stem: stem.to_string(),
                contents,
            });
        }
    }
    build_collection_view(collection, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collection(group_by: Option<&str>) -> RoutineCollection {
        RoutineCollection {
            id: "books".into(),
            name: "Books".into(),
            path: "reference/readwise/books".into(),
            group_by: group_by.map(str::to_string),
        }
    }

    fn note(stem: &str, contents: &str) -> CollectionNote {
        CollectionNote {
            stem: stem.into(),
            contents: contents.into(),
        }
    }

    fn readwise_note(title: &str, author: &str) -> String {
        format!(
            "---\nsource: readwise\nreadwise_id: 1\n---\n# {title}\n\n## Metadata\n\
             - Author: [[{author}]]\n- Full Title: {title}\n\n## Highlights\n- x\n"
        )
    }

    fn titles(entries: &[CollectionEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.title.as_str()).collect()
    }

    #[test]
    fn flat_collections_list_titles_alphabetically() {
        let view = build_collection_view(
            &collection(None),
            vec![
                note("zen", &readwise_note("Zen and the Art", "Pirsig")),
                note(
                    "a-fe",
                    &readwise_note("A Fé Na Era Do Ceticismo", "Timothy Keller"),
                ),
                note("untitled", "just text\n"),
            ],
        );
        let CollectionItems::Flat(entries) = &view.items else {
            panic!("expected a flat collection");
        };
        assert_eq!(
            titles(entries),
            ["A Fé Na Era Do Ceticismo", "untitled", "Zen and the Art"]
        );
        assert_eq!(entries[0].link_id, "books/a-fe");
        assert_eq!(entries[0].rel_path, "reference/readwise/books/a-fe.md");
    }

    #[test]
    fn grouped_collections_bucket_by_field() {
        let view = build_collection_view(
            &collection(Some("author")),
            vec![
                note("b", &readwise_note("Reason for God", "Timothy Keller")),
                note("a", &readwise_note("A Fé", "Timothy Keller")),
                note("c", &readwise_note("Mistborn", "Brandon Sanderson")),
                note(
                    "d",
                    "---\nauthor: \"Morgan Housel\"\n---\n# Psychology of Money\n",
                ),
                note("e", "# No metadata\n"),
            ],
        );
        let CollectionItems::Grouped(groups) = &view.items else {
            panic!("expected a grouped collection");
        };
        let shape: Vec<(&str, Vec<&str>)> = groups
            .iter()
            .map(|group| (group.label.as_str(), titles(&group.entries)))
            .collect();
        assert_eq!(
            shape,
            [
                ("Brandon Sanderson", vec!["Mistborn"]),
                ("Morgan Housel", vec!["Psychology of Money"]),
                ("Timothy Keller", vec!["A Fé", "Reason for God"]),
                ("No author", vec!["No metadata"]),
            ]
        );
        assert_eq!(view.entries().count(), 5);
    }

    #[test]
    fn fields_prefer_frontmatter_and_unwrap_links() {
        assert_eq!(
            note_field("---\nAuthor: Front\n---\n- Author: Body\n", "author").as_deref(),
            Some("Front")
        );
        assert_eq!(
            note_field("# T\n- Author: [[Target|Alias]]\n", "author").as_deref(),
            Some("Target")
        );
        assert_eq!(note_field("# T\n- Author:   \n", "author"), None);
        assert_eq!(
            note_field("# T\nAuthor: prose, not a list\n", "author"),
            None
        );
        assert_eq!(
            note_title("---\ntitle: x\n---\n## Not this\n# This one\n").as_deref(),
            Some("This one")
        );
    }
}
