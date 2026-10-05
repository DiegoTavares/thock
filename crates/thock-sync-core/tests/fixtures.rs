//! The language-neutral fixtures of spec §9.2, and the generator that writes
//! them. `cargo test -p thock_sync_core` verifies the files on disk; running
//! with `THOCK_SYNC_CORE_WRITE_FIXTURES=1` regenerates the `gen-*` files after
//! asserting every hand-stated expectation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use thock_sync_core::{
    Applied, Context, Heading, Operation, Outcome, Placement, Write, apply, content_hash,
    effect_present, heading_key, is_syncable_path, key_check, line_hash, open, parse_write,
    seal_with_nonce, section_hash,
};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/v1")
}

fn writing() -> bool {
    std::env::var_os("THOCK_SYNC_CORE_WRITE_FIXTURES").is_some()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Case {
    name: String,
    before: Option<String>,
    seed: Option<String>,
    write: Option<serde_json::Value>,
    after: String,
    outcome: Outcome,
    effect_present_before: bool,
    effect_present_after: bool,
}

const AREAS: [&str; 8] = [
    "append",
    "create",
    "replace_line",
    "remove_line",
    "replace_section",
    "headings",
    "line_endings",
    "roundtrip",
];

/// The probe a `roundtrip` case applies: a write with nothing to add, which
/// must hand the bytes back untouched.
fn roundtrip_probe() -> Write {
    Write::new(
        "roundtrip",
        "note.md",
        Operation::Append {
            heading: None,
            lines: Vec::new(),
            placement: Placement::End,
            blank_line_before: false,
            create_from_template: false,
        },
    )
}

fn run_case(area: &str, file: &Path, case: &Case) {
    let label = format!("{area}/{}", file.display());
    let write = match &case.write {
        Some(value) => parse_write(&value.to_string())
            .unwrap_or_else(|error| panic!("{label}: write doesn't parse: {error}")),
        None => roundtrip_probe(),
    };
    let before = case.before.as_deref();
    if let Some(text) = before {
        assert_eq!(
            effect_present(text, &write),
            case.effect_present_before,
            "{label}: effect_present(before)"
        );
    }
    let applied = apply(before, &write, case.seed.as_deref());
    assert_eq!(applied.text, case.after, "{label}: after");
    assert_eq!(applied.outcome, case.outcome, "{label}: outcome");
    assert_eq!(
        effect_present(&applied.text, &write),
        case.effect_present_after,
        "{label}: effect_present(after)"
    );
    let again = apply(Some(&applied.text), &write, case.seed.as_deref());
    assert_eq!(
        again.text, applied.text,
        "{label}: second application changed the text"
    );
    assert_eq!(
        again.outcome,
        Outcome::Noop,
        "{label}: second application wasn't a noop"
    );
    if case.write.is_none() {
        assert_eq!(
            applied.text,
            case.before.clone().unwrap_or_default(),
            "{label}: roundtrip"
        );
    }
}

#[test]
fn every_fixture_passes() {
    if writing() {
        return;
    }
    let mut total = 0;
    for area in AREAS {
        let dir = fixtures_root().join(area);
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        entries.sort();
        assert!(!entries.is_empty(), "no fixtures under {}", dir.display());
        for path in entries {
            let text = fs::read_to_string(&path).expect("fixture readable");
            let case: Case = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            run_case(area, &path, &case);
            total += 1;
        }
    }
    assert!(total > 100, "only {total} fixtures");
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum HashVector {
    Line {
        line: String,
        line_hash: String,
    },
    Heading {
        text: String,
        heading_key: String,
    },
    Section {
        body: Vec<String>,
        section_hash: String,
    },
}

#[test]
fn hash_vectors_match() {
    if writing() {
        return;
    }
    let text = fs::read_to_string(fixtures_root().join("hashes.json")).expect("hashes.json");
    let vectors: Vec<HashVector> = serde_json::from_str(&text).expect("hashes.json parses");
    assert!(vectors.len() >= 30);
    for vector in vectors {
        match vector {
            HashVector::Line {
                line,
                line_hash: expected,
            } => {
                assert_eq!(line_hash(&line), expected, "line_hash({line:?})");
            }
            HashVector::Heading {
                text,
                heading_key: expected,
            } => {
                assert_eq!(heading_key(&text), expected, "heading_key({text:?})");
            }
            HashVector::Section {
                body,
                section_hash: expected,
            } => {
                let note = format!("## S\n{}\n", body.join("\n"));
                assert_eq!(
                    section_hash(&note, &Heading::new("S")),
                    Some(expected),
                    "section_hash({body:?})"
                );
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum EnvelopeVector {
    Sealed {
        key: String,
        nonce: String,
        context: serde_json::Value,
        plaintext: String,
        envelope: String,
        content_hash: String,
    },
    KeyCheck {
        key: String,
        key_check: String,
    },
}

fn context_from_value(value: &serde_json::Value) -> Context {
    match value.get("kind").and_then(|kind| kind.as_str()) {
        Some("file") => Context::File {
            path: value["path"].as_str().unwrap_or_default().to_string(),
            blob_id: value["blob_id"].as_str().unwrap_or_default().to_string(),
        },
        Some("write") => Context::Write {
            client_id: value["client_id"].as_str().unwrap_or_default().to_string(),
        },
        other => panic!("unknown context kind {other:?}"),
    }
}

fn hex32(text: &str) -> [u8; 32] {
    let bytes = hex::decode(text).expect("hex key");
    bytes.try_into().expect("32 bytes")
}

#[test]
fn envelope_vectors_match() {
    if writing() {
        return;
    }
    let text = fs::read_to_string(fixtures_root().join("envelope.json")).expect("envelope.json");
    let vectors: Vec<EnvelopeVector> = serde_json::from_str(&text).expect("envelope.json parses");
    let standard = base64::engine::general_purpose::STANDARD;
    let mut sealed = 0;
    let mut checks = 0;
    for vector in vectors {
        match vector {
            EnvelopeVector::Sealed {
                key,
                nonce,
                context,
                plaintext,
                envelope,
                content_hash: expected_hash,
            } => {
                let key = hex32(&key);
                let nonce: [u8; 12] = hex::decode(nonce)
                    .expect("hex nonce")
                    .try_into()
                    .expect("12 bytes");
                let plaintext = standard.decode(plaintext).expect("base64 plaintext");
                let envelope = standard.decode(envelope).expect("base64 envelope");
                let context = context_from_value(&context);
                assert_eq!(
                    seal_with_nonce(&key, &nonce, context.clone(), &plaintext),
                    envelope,
                    "seal {context:?}"
                );
                assert_eq!(
                    open(&key, context.clone(), &envelope),
                    Ok(plaintext),
                    "open {context:?}"
                );
                assert_eq!(content_hash(&envelope), expected_hash);
                let moved = match &context {
                    Context::File { path, .. } => Context::File {
                        path: format!("{path}.moved"),
                        blob_id: "00000000000000000000000000000000".into(),
                    },
                    Context::Write { client_id } => Context::Write {
                        client_id: format!("{client_id}-other"),
                    },
                };
                assert!(open(&key, moved, &envelope).is_err());
                sealed += 1;
            }
            EnvelopeVector::KeyCheck {
                key,
                key_check: expected,
            } => {
                assert_eq!(key_check(&hex32(&key)), expected);
                checks += 1;
            }
        }
    }
    assert!(sealed >= 3 && checks >= 1);
}

#[derive(Debug, Deserialize)]
struct PathVectors {
    syncable: Vec<String>,
    refused: Vec<String>,
}

#[test]
fn path_vectors_match() {
    let text = fs::read_to_string(fixtures_root().join("paths.json")).expect("paths.json");
    let vectors: PathVectors = serde_json::from_str(&text).expect("paths.json parses");
    assert!(!vectors.syncable.is_empty() && !vectors.refused.is_empty());
    for path in &vectors.syncable {
        assert!(is_syncable_path(path), "{path:?} should sync");
    }
    for path in &vectors.refused {
        assert!(!is_syncable_path(path), "{path:?} should be refused");
    }
}

// ---------------------------------------------------------------------------
// The generator.

struct Note {
    id: &'static str,
    area: &'static str,
    text: String,
    /// The section the matrix writes into and the line it edits there.
    heading: &'static str,
    target_line: &'static str,
    /// A line with the same normalised text as `target_line` once edited.
    edited_line: &'static str,
    /// What the kept-both path edits: a line that is not in the note.
    absent_line: &'static str,
}

const DAILY_TEMPLATE: &str = "# Thursday, October 2, 2026\n\n_A page for today. Write a little or a lot; it's yours._\n\n___\n\n## Journal\n\n_What happened, what you noticed, how it went._\n\n___\n\n## Day planner\n\n_Timed lines land on the planner beside you, like `- [ ] 09:00 - 10:00 Deep work`._\n\n___\n\n## Personal\n\n_The people, the errands, the small good things._\n";

const WEEKLY_TEMPLATE: &str = "# Week 40, 2026\n\n_Seven days, one page. Set a direction at the start, look back at the end._\n\n___\n\n## Goals\n\n_Two or three things that would make this a good week._\n\n- [ ] Ship the sync spec\n- [ ] Call Mum\n\n___\n\n## Notes\n\n_Anything worth keeping that doesn't belong to a single day._\n\n___\n\n## Week review\n\n_How did it go? The **Week Review** ritual appends its take below yours._\n";

const EXAMPLE_DAY: &str = "# Thursday, October 2, 2026\n\n> 👋 **This first day is an example.** It shows what a daily note can hold:\n> sections, lists, checkboxes, a few emojis. Change it, delete it, keep the\n> bits you like. It's your page now.\n>\n> Tomorrow's note starts from `templates/daily.md`, and that template is\n> meant to be customized: open it (or ask your agent) and make it yours.\n\n___\n\n## Journal\n\n_What happened, what you noticed, how it went._\n\nSlept badly, but the morning walk fixed it 🌤️. Coffee with **Ana**, who is\nthinking about changing jobs. I said I'd send her that article.\n\nNoticed I keep saying yes to things on Thursdays. Worth watching.\n\n___\n\n## Day planner\n\n_Timed lines land on the planner beside you, like `- [ ] 09:00 - 10:00 Deep work`._\n\n- [x] 08:00 - 08:30 Morning walk 🚶\n- [ ] 09:30 - 11:00 Deep work: the budget spreadsheet 📊\n- [ ] 12:30 Lunch with Ana 🥗\n- [ ] 15:00 - 15:30 Call the dentist ☎️\n- [ ] Buy a birthday card for Dad 🎂\n- [ ] Read 20 pages 📖\n\n___\n\n## Personal\n\n_The people, the errands, the small good things._\n\n- Groceries: eggs, spinach, lemons 🍋\n- Idea 💡: a weekly no-plans Sunday\n- Grateful for: the neighbour who watered the plants\n\n> \"The days are long, but the years are short.\"\n\n___\n\n## How this page works\n\n- `- [ ]` makes a checkbox. Put a time in front and it appears on the planner to the right.\n- `## ` starts a section, `___` draws a line, `**bold**` and `_italic_` do what they say, and `> ` quotes.\n- Double brackets link to another note: [[welcome]].\n- Your agent adds its part *below* yours and never rewrites what you wrote. Try **Wrap Today** in the left rail tonight.\n";

const FRONT_MATTER: &str = "---\ntags: [daily, phone]\n# a YAML comment, not a heading\nmood: 7\n---\n# Friday\n\n## Journal\n\n**08:10** · Up early.\n\n## Day planner\n\n- [ ] 09:00 - 10:00 Standup\n- [ ] Write the report\n";

const FENCED: &str = "# Notes\n\n## Snippets\n\n```md\n## not a heading\n- [ ] not a task either\n```\n\n~~~\n### also fenced\n~~~\n\n- [ ] Review the snippet\n\n## Tasks\n\n- [ ] Ship it\n";

const RICH: &str = "# Rich\n\n## Table\n\n| Name | Due |\n| --- | --- |\n| Report | Friday |\n\n<!-- a comment the phone never touches -->\n\n## Lists\n\n- Parent\n  - Child one\n  - Child two\n    - Grandchild\n- [ ] 14:00 Sibling task <!--gcal:aaaaaaaaaaaa-->\n\n## Personal\n\nProse.\n";

const CRLF: &str = "# Monday\r\n\r\n## Journal\r\n\r\nRainy.\r\n\r\n## Day planner\r\n\r\n- [ ] 09:00 - 09:30 Standup\r\n- [ ] Groceries\r\n";

const NO_FINAL_NEWLINE: &str =
    "# Tuesday\n\n## Day planner\n\n- [ ] Call the bank\n- [ ] Water the plants";

const DUPLICATES: &str = "# Wednesday\n\n## Day planner\n\n- [ ] Call Ana\n- [ ] Walk\n- [ ] Call Ana\n- [x] Walk\n- [ ] Email Bea\n";

const AGENT_HEADINGS: &str = "# Thursday\n\n## Journal\n\nQuiet day.\n\n## Day planner\n\n- [ ] 09:30 - 11:00 Deep work\n\n### Calendar\n\n- [ ] 12:30 - 13:30 Lunch with Ana <!--gcal:bbbbbbbbbbbb-->\n\n# Daily Closure\n\nThe agent's summary of the day.\n\n# AI Week Review\n\nNot really here on a daily note, but the rule must still hold.\n";

fn notes() -> Vec<Note> {
    vec![
        Note {
            id: "daily-template",
            area: "",
            text: DAILY_TEMPLATE.into(),
            heading: "Day planner",
            target_line: "_Timed lines land on the planner beside you, like `- [ ] 09:00 - 10:00 Deep work`._",
            edited_line: "_Timed lines land on the planner beside you, like `- [ ] 09:00 - 10:00 Deep work`._ <!--seen-->",
            absent_line: "- [ ] A task that was never here",
        },
        Note {
            id: "weekly-template",
            area: "",
            text: WEEKLY_TEMPLATE.into(),
            heading: "Goals",
            target_line: "- [ ] Ship the sync spec",
            edited_line: "- [x] Ship the sync spec",
            absent_line: "- [x] A goal the desk removed",
        },
        Note {
            id: "example-day",
            area: "",
            text: EXAMPLE_DAY.into(),
            heading: "Day planner",
            target_line: "- [ ] 09:30 - 11:00 Deep work: the budget spreadsheet 📊",
            edited_line: "- [x] 09:30 - 11:00 Deep work: the budget spreadsheet 📊",
            absent_line: "- [ ] 16:00 Something the desk deleted",
        },
        Note {
            id: "front-matter",
            area: "headings",
            text: FRONT_MATTER.into(),
            heading: "Day planner",
            target_line: "- [ ] Write the report",
            edited_line: "- [ ] 16:00 - 17:00 Write the report",
            absent_line: "- [ ] Gone",
        },
        Note {
            id: "fenced",
            area: "headings",
            text: FENCED.into(),
            heading: "Snippets",
            target_line: "- [ ] Review the snippet",
            edited_line: "- [x] Review the snippet",
            absent_line: "- [ ] not a task either",
        },
        Note {
            id: "rich",
            area: "",
            text: RICH.into(),
            heading: "Lists",
            target_line: "- [ ] 14:00 Sibling task <!--gcal:aaaaaaaaaaaa-->",
            edited_line: "- [x] 14:00 Sibling task <!--gcal:aaaaaaaaaaaa-->",
            absent_line: "- [ ] Cousin task",
        },
        Note {
            id: "crlf",
            area: "line_endings",
            text: CRLF.into(),
            heading: "Day planner",
            target_line: "- [ ] Groceries",
            edited_line: "- [x] Groceries",
            absent_line: "- [ ] Laundry",
        },
        Note {
            id: "no-final-newline",
            area: "line_endings",
            text: NO_FINAL_NEWLINE.into(),
            heading: "Day planner",
            target_line: "- [ ] Water the plants",
            edited_line: "- [x] Water the plants",
            absent_line: "- [ ] Feed the cat",
        },
        Note {
            id: "duplicates",
            area: "",
            text: DUPLICATES.into(),
            heading: "Day planner",
            target_line: "- [ ] Email Bea",
            edited_line: "- [x] Email Bea",
            absent_line: "- [ ] Call Bea",
        },
        Note {
            id: "agent-headings",
            area: "headings",
            text: AGENT_HEADINGS.into(),
            heading: "Day planner",
            target_line: "- [ ] 09:30 - 11:00 Deep work",
            edited_line: "- [x] 09:30 - 11:00 Deep work",
            absent_line: "- [ ] 17:00 Gym",
        },
    ]
}

fn make_write(note: &Note, kind: &str, operation: Operation) -> Write {
    let mut write = Write::new(
        format!("fixture-{}-{kind}", note.id),
        "daily/2026-10-02.md",
        operation,
    );
    write.made_at = "2026-10-02T13:58:02Z".into();
    write.device_id = "c41a2f8e9b7d1c35".into();
    write
}

fn heading_of(note: &Note) -> Option<Heading> {
    Some(Heading::new(note.heading))
}

fn append_op(
    heading: Option<Heading>,
    lines: &[&str],
    placement: Placement,
    blank: bool,
    template: bool,
) -> Operation {
    Operation::Append {
        heading,
        lines: lines.iter().map(|line| line.to_string()).collect(),
        placement,
        blank_line_before: blank,
        create_from_template: template,
    }
}

fn body_of(note: &Note, heading: &str) -> Vec<String> {
    // A fence- and front-matter-aware walk, independent of the crate, so the
    // generator's idea of a body is not the implementation's.
    let lines: Vec<&str> = note
        .text
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .collect();
    let lines = if note.text.ends_with('\n') {
        &lines[..lines.len().saturating_sub(1)]
    } else {
        &lines[..]
    };
    let mut in_section = false;
    let mut level = 0usize;
    let mut fence: Option<char> = None;
    let mut front_matter = lines.first() == Some(&"---");
    let mut body: Vec<String> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if front_matter {
            if index > 0 && (*line == "---" || *line == "...") {
                front_matter = false;
            }
            if in_section {
                body.push(line.to_string());
            }
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(&open.to_string().repeat(3)) {
                fence = None;
            }
            if in_section {
                body.push(line.to_string());
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = trimmed.chars().next();
            if in_section {
                body.push(line.to_string());
            }
            continue;
        }
        let hashes = line.chars().take_while(|c| *c == '#').count();
        let is_heading = hashes > 0 && hashes <= 6 && line[hashes..].starts_with(' ');
        if is_heading {
            if in_section && hashes <= level {
                break;
            }
            if !in_section && heading_key(&line[hashes..]) == heading_key(heading) {
                in_section = true;
                level = hashes;
                continue;
            }
        }
        if in_section {
            body.push(line.to_string());
        }
    }
    while body.last().is_some_and(|line| line.trim().is_empty()) {
        body.pop();
    }
    if body
        .last()
        .is_some_and(|line| line.trim() == "___" || line.trim() == "---")
    {
        body.pop();
        while body.last().is_some_and(|line| line.trim().is_empty()) {
            body.pop();
        }
    }
    body
}

/// A case whose `after` is recorded from the implementation, with the
/// outcome and presence flags stated up front so the generator catches a
/// rule that drifts.
struct Spec {
    area: &'static str,
    name: String,
    before: Option<String>,
    seed: Option<String>,
    write: Option<Write>,
    outcome: Outcome,
    present_before: bool,
    present_after: bool,
    /// When stated, the exact text the application must produce.
    expected: Option<String>,
}

fn matrix(note: &Note) -> Vec<Spec> {
    let text = note.text.clone();
    let heading = heading_of(note);
    let body = body_of(note, note.heading);
    let target_hash = line_hash(note.target_line);
    let absent_hash = line_hash(note.absent_line);
    let body_hash = {
        let joined = body.join("\n");
        let mut hex = hex::encode(sha2::Sha256::digest(joined.as_bytes()));
        hex.truncate(16);
        assert_eq!(
            section_hash(&note.text, &Heading::new(note.heading)),
            Some(hex.clone()),
            "{}: the generator and the crate disagree on the body of {}",
            note.id,
            note.heading
        );
        hex
    };
    let area_or = |default: &'static str| {
        if note.area.is_empty() {
            default
        } else {
            note.area
        }
    };
    let mut specs = Vec::new();
    let mut push = |area: &'static str,
                    state: &str,
                    kind: &str,
                    before: Option<String>,
                    seed: Option<String>,
                    write: Write,
                    outcome: Outcome,
                    before_present: bool,
                    after_present: bool| {
        specs.push(Spec {
            area,
            name: format!("{}-{kind}-{state}", note.id),
            before,
            seed,
            write: Some(write),
            outcome,
            present_before: before_present,
            present_after: after_present,
            expected: None,
        });
    };

    // append
    let a = area_or("append");
    push(
        a,
        "present",
        "append",
        Some(text.clone()),
        None,
        make_write(
            note,
            "append",
            append_op(
                heading.clone(),
                &[note.target_line],
                Placement::End,
                false,
                false,
            ),
        ),
        Outcome::Noop,
        true,
        true,
    );
    push(
        a,
        "end",
        "append",
        Some(text.clone()),
        None,
        make_write(
            note,
            "append",
            append_op(
                heading.clone(),
                &["- [ ] Captured on the phone"],
                Placement::End,
                false,
                false,
            ),
        ),
        Outcome::Applied,
        false,
        true,
    );
    push(
        a,
        "before-children",
        "append",
        Some(text.clone()),
        None,
        make_write(
            note,
            "append",
            append_op(
                heading.clone(),
                &["- [ ] Above the subsections"],
                Placement::BeforeChildren,
                false,
                false,
            ),
        ),
        Outcome::Applied,
        false,
        true,
    );
    push(
        a,
        "prose",
        "append",
        Some(text.clone()),
        None,
        make_write(
            note,
            "append",
            append_op(
                Some(Heading::new("Journal")),
                &["**21:14** · A thought typed on the phone."],
                Placement::End,
                true,
                false,
            ),
        ),
        if body_of(note, "Journal").is_empty() && !text.contains("## Journal") {
            Outcome::SectionAdded
        } else {
            Outcome::Applied
        },
        false,
        true,
    );
    push(
        a,
        "end-of-file",
        "append",
        Some(text.clone()),
        None,
        make_write(
            note,
            "append",
            append_op(
                None,
                &["Appended at the very end."],
                Placement::End,
                true,
                false,
            ),
        ),
        Outcome::Applied,
        false,
        true,
    );
    push(
        a,
        "section-missing",
        "append",
        Some(text.clone()),
        None,
        make_write(
            note,
            "append",
            append_op(
                Some(Heading {
                    text: "Asked on the go".into(),
                    level: 1,
                    ordinal: 0,
                }),
                &["The agent's answer, kept."],
                Placement::End,
                false,
                false,
            ),
        ),
        Outcome::SectionAdded,
        false,
        true,
    );
    push(
        a,
        "file-missing-seeded",
        "append",
        None,
        Some(DAILY_TEMPLATE.into()),
        make_write(
            note,
            "append",
            append_op(
                heading.clone(),
                &["- [ ] First line of a new day"],
                Placement::End,
                false,
                true,
            ),
        ),
        Outcome::Created,
        false,
        true,
    );
    push(
        a,
        "file-missing-bare",
        "append",
        None,
        None,
        make_write(
            note,
            "append",
            append_op(
                heading.clone(),
                &["- [ ] First line, no template"],
                Placement::End,
                false,
                false,
            ),
        ),
        Outcome::Created,
        false,
        true,
    );

    // create
    let c = area_or("create");
    push(
        c,
        "file-missing",
        "create",
        None,
        None,
        make_write(
            note,
            "create",
            Operation::Create {
                content: text.clone(),
            },
        ),
        Outcome::Created,
        false,
        true,
    );
    push(
        c,
        "same",
        "create",
        Some(text.clone()),
        None,
        make_write(
            note,
            "create",
            Operation::Create {
                content: text.clone(),
            },
        ),
        Outcome::Noop,
        true,
        true,
    );
    push(c, "different", "create", Some(text.clone()), None,
        make_write(note, "create", Operation::Create { content: "---\nsource: thock-ios\ncapture: 4d1f9a02c7b3\n---\n\n# A capture\n\nIts text.\n".into() }),
        Outcome::Applied, false, true);

    // replace_line
    let r = area_or("replace_line");
    push(
        r,
        "present",
        "replace_line",
        Some(text.clone()),
        None,
        make_write(
            note,
            "replace_line",
            Operation::ReplaceLine {
                heading: heading.clone(),
                line_hash: target_hash.clone(),
                ordinal: 0,
                new_line: note.target_line.into(),
            },
        ),
        Outcome::Noop,
        true,
        true,
    );
    push(
        r,
        "found",
        "replace_line",
        Some(text.clone()),
        None,
        make_write(
            note,
            "replace_line",
            Operation::ReplaceLine {
                heading: heading.clone(),
                line_hash: target_hash.clone(),
                ordinal: 0,
                new_line: note.edited_line.into(),
            },
        ),
        Outcome::Applied,
        false,
        true,
    );
    push(
        r,
        "changed",
        "replace_line",
        Some(text.clone()),
        None,
        make_write(
            note,
            "replace_line",
            Operation::ReplaceLine {
                heading: heading.clone(),
                line_hash: absent_hash.clone(),
                ordinal: 0,
                new_line: note.absent_line.into(),
            },
        ),
        Outcome::KeptBoth,
        false,
        true,
    );
    push(
        r,
        "section-missing",
        "replace_line",
        Some(text.clone()),
        None,
        make_write(
            note,
            "replace_line",
            Operation::ReplaceLine {
                heading: Some(Heading::new("Errands")),
                line_hash: absent_hash.clone(),
                ordinal: 0,
                new_line: note.absent_line.into(),
            },
        ),
        Outcome::SectionAdded,
        false,
        true,
    );
    push(
        r,
        "file-missing",
        "replace_line",
        None,
        None,
        make_write(
            note,
            "replace_line",
            Operation::ReplaceLine {
                heading: heading.clone(),
                line_hash: target_hash.clone(),
                ordinal: 0,
                new_line: note.edited_line.into(),
            },
        ),
        Outcome::Created,
        false,
        true,
    );

    // remove_line
    let m = area_or("remove_line");
    push(
        m,
        "found",
        "remove_line",
        Some(text.clone()),
        None,
        make_write(
            note,
            "remove_line",
            Operation::RemoveLine {
                heading: heading.clone(),
                line_hash: target_hash.clone(),
                ordinal: 0,
            },
        ),
        Outcome::Applied,
        false,
        true,
    );
    push(
        m,
        "missing",
        "remove_line",
        Some(text.clone()),
        None,
        make_write(
            note,
            "remove_line",
            Operation::RemoveLine {
                heading: heading.clone(),
                line_hash: absent_hash,
                ordinal: 0,
            },
        ),
        Outcome::Noop,
        true,
        true,
    );
    push(
        m,
        "section-missing",
        "remove_line",
        Some(text.clone()),
        None,
        make_write(
            note,
            "remove_line",
            Operation::RemoveLine {
                heading: Some(Heading::new("Errands")),
                line_hash: target_hash.clone(),
                ordinal: 0,
            },
        ),
        Outcome::Noop,
        true,
        true,
    );
    push(
        m,
        "file-missing",
        "remove_line",
        None,
        None,
        make_write(
            note,
            "remove_line",
            Operation::RemoveLine {
                heading: heading,
                line_hash: target_hash,
                ordinal: 0,
            },
        ),
        Outcome::Created,
        false,
        true,
    );

    // replace_section
    let s = area_or("replace_section");
    let mut edited_body = body.clone();
    edited_body.push("- [ ] A line added while editing the section".into());
    let edited_body_strs: Vec<&str> = edited_body.iter().map(String::as_str).collect();
    let section_heading = Heading::new(note.heading);
    push(
        s,
        "fresh",
        "replace_section",
        Some(text.clone()),
        None,
        make_write(
            note,
            "replace_section",
            Operation::ReplaceSection {
                heading: section_heading.clone(),
                base_hash: body_hash,
                lines: edited_body.clone(),
            },
        ),
        Outcome::Applied,
        false,
        true,
    );
    push(
        s,
        "present",
        "replace_section",
        Some(text.clone()),
        None,
        make_write(
            note,
            "replace_section",
            Operation::ReplaceSection {
                heading: section_heading.clone(),
                base_hash: "0000000000000000".into(),
                lines: body,
            },
        ),
        Outcome::Noop,
        true,
        true,
    );
    push(
        s,
        "stale",
        "replace_section",
        Some(text.clone()),
        None,
        make_write(
            note,
            "replace_section",
            Operation::ReplaceSection {
                heading: section_heading.clone(),
                base_hash: "0000000000000000".into(),
                lines: edited_body_strs
                    .iter()
                    .map(|line| line.to_string())
                    .collect(),
            },
        ),
        Outcome::KeptBoth,
        false,
        true,
    );
    push(
        s,
        "section-missing",
        "replace_section",
        Some(text.clone()),
        None,
        make_write(
            note,
            "replace_section",
            Operation::ReplaceSection {
                heading: Heading::new("Errands"),
                base_hash: line_hash_of_empty(),
                lines: vec!["- [ ] Post the letter".into()],
            },
        ),
        Outcome::SectionAdded,
        false,
        true,
    );
    push(
        s,
        "file-missing",
        "replace_section",
        None,
        None,
        make_write(
            note,
            "replace_section",
            Operation::ReplaceSection {
                heading: section_heading,
                base_hash: line_hash_of_empty(),
                lines: vec!["- [ ] Only line".into()],
            },
        ),
        Outcome::Created,
        false,
        true,
    );

    // roundtrip
    specs.push(Spec {
        area: "roundtrip",
        name: format!("{}-untouched", note.id),
        before: Some(text.clone()),
        seed: None,
        write: None,
        outcome: Outcome::Noop,
        present_before: true,
        present_after: true,
        expected: Some(text),
    });
    specs
}

/// `section_hash` of an empty body.
fn line_hash_of_empty() -> String {
    let mut hex = hex::encode(sha2::Sha256::digest(b""));
    hex.truncate(16);
    hex
}

/// Cases whose exact output is stated by hand, from the spec's own examples
/// and the corners the rules exist for.
fn hand_cases() -> Vec<Spec> {
    fn w(id: &str, path: &str, operation: Operation) -> Write {
        let mut write = Write::new(format!("hand-{id}"), path, operation);
        write.made_at = "2026-10-02T13:58:02Z".into();
        write.device_id = "c41a2f8e9b7d1c35".into();
        write
    }
    #[allow(clippy::too_many_arguments)]
    fn spec(
        area: &'static str,
        name: &str,
        before: Option<&str>,
        seed: Option<&str>,
        write: Write,
        outcome: Outcome,
        present_before: bool,
        after: &str,
    ) -> Spec {
        Spec {
            area,
            name: name.into(),
            before: before.map(str::to_string),
            seed: seed.map(str::to_string),
            write: Some(write),
            outcome,
            present_before,
            present_after: true,
            expected: Some(after.into()),
        }
    }
    let planner = || Some(Heading::new("Day planner"));
    vec![
        spec(
            "replace_line",
            "spec-example-tick-after-retime",
            Some("## Day planner\n- [ ] 09:30 - 11:00 Deep work\n"),
            None,
            w(
                "tick",
                "daily/2026-10-02.md",
                Operation::ReplaceLine {
                    heading: planner(),
                    line_hash: line_hash("Deep work"),
                    ordinal: 0,
                    new_line: "- [x] Deep work".into(),
                },
            ),
            Outcome::Applied,
            false,
            "## Day planner\n- [x] Deep work\n",
        ),
        spec(
            "replace_line",
            "tick-finds-a-line-the-desk-retimed",
            Some("## Day planner\n- [ ] 10:00 - 11:30 Deep work\n"),
            None,
            w(
                "tick2",
                "daily/2026-10-02.md",
                Operation::ReplaceLine {
                    heading: planner(),
                    line_hash: line_hash("- [ ] 09:30 - 11:00 Deep work"),
                    ordinal: 0,
                    new_line: "- [x] 09:30 - 11:00 Deep work".into(),
                },
            ),
            Outcome::Applied,
            false,
            "## Day planner\n- [x] 09:30 - 11:00 Deep work\n",
        ),
        spec(
            "replace_line",
            "same-line-edited-at-both-ends-keeps-both",
            Some("## Day planner\n- [ ] Call the dentist at 15:00\n"),
            None,
            w(
                "both",
                "daily/2026-10-02.md",
                Operation::ReplaceLine {
                    heading: planner(),
                    line_hash: line_hash("Call the dentist"),
                    ordinal: 0,
                    new_line: "- [ ] Call the dentist, ask about Friday".into(),
                },
            ),
            Outcome::KeptBoth,
            false,
            "## Day planner\n- [ ] Call the dentist at 15:00\n- [ ] Call the dentist, ask about Friday <!--thock:also-->\n",
        ),
        spec(
            "replace_line",
            "ordinal-picks-the-second-duplicate",
            Some("## Day planner\n- [ ] Call Ana\n- [ ] Walk\n- [ ] Call Ana\n"),
            None,
            w(
                "dup",
                "daily/2026-10-02.md",
                Operation::ReplaceLine {
                    heading: planner(),
                    line_hash: line_hash("Call Ana"),
                    ordinal: 1,
                    new_line: "- [x] Call Ana".into(),
                },
            ),
            Outcome::Applied,
            false,
            "## Day planner\n- [ ] Call Ana\n- [ ] Walk\n- [x] Call Ana\n",
        ),
        spec(
            "replace_line",
            "tick-the-second-of-two-identical-lines",
            Some("## Day planner\n- [x] Buy milk\n- [ ] Buy milk\n"),
            None,
            w(
                "dup-tick",
                "daily/2026-10-02.md",
                Operation::ReplaceLine {
                    heading: planner(),
                    line_hash: line_hash("Buy milk"),
                    ordinal: 1,
                    new_line: "- [x] Buy milk".into(),
                },
            ),
            Outcome::Applied,
            false,
            "## Day planner\n- [x] Buy milk\n- [x] Buy milk\n",
        ),
        spec(
            "append",
            "end-of-file-is-below-a-closing-rule",
            Some("# Title\n\n## Journal\nfoo\n\n___\n"),
            None,
            w(
                "eof-rule",
                "daily/2026-10-02.md",
                Operation::Append {
                    heading: None,
                    lines: vec!["tail".into()],
                    placement: Placement::End,
                    blank_line_before: false,
                    create_from_template: false,
                },
            ),
            Outcome::Applied,
            false,
            "# Title\n\n## Journal\nfoo\n\n___\ntail\n",
        ),
        spec(
            "replace_line",
            "ordinal-out-of-range-uses-the-first",
            Some("## Day planner\n- [ ] Call Ana\n- [ ] Call Ana\n"),
            None,
            w(
                "dup2",
                "daily/2026-10-02.md",
                Operation::ReplaceLine {
                    heading: planner(),
                    line_hash: line_hash("Call Ana"),
                    ordinal: 5,
                    new_line: "- [x] Call Ana".into(),
                },
            ),
            Outcome::Applied,
            false,
            "## Day planner\n- [x] Call Ana\n- [ ] Call Ana\n",
        ),
        spec(
            "append",
            "append-stays-above-the-closing-rule",
            Some("## Day planner\n\n- [ ] Walk\n\n___\n\n## Personal\n"),
            None,
            w(
                "rule",
                "daily/2026-10-02.md",
                append_op(
                    planner(),
                    &["- [ ] Buy a card"],
                    Placement::End,
                    false,
                    false,
                ),
            ),
            Outcome::Applied,
            false,
            "## Day planner\n\n- [ ] Walk\n- [ ] Buy a card\n\n___\n\n## Personal\n",
        ),
        spec(
            "append",
            "append-end-lands-below-the-calendar-child",
            Some(
                "## Day planner\n- [ ] Walk\n\n### Calendar\n- [ ] 12:30 Lunch <!--gcal:a-->\n\n## Personal\n",
            ),
            None,
            w(
                "end",
                "daily/2026-10-02.md",
                append_op(
                    planner(),
                    &["- [ ] Buy a card"],
                    Placement::End,
                    false,
                    false,
                ),
            ),
            Outcome::Applied,
            false,
            "## Day planner\n- [ ] Walk\n\n### Calendar\n- [ ] 12:30 Lunch <!--gcal:a-->\n- [ ] Buy a card\n\n## Personal\n",
        ),
        spec(
            "append",
            "append-before-children-stays-with-the-planner",
            Some(
                "## Day planner\n- [ ] Walk\n\n### Calendar\n- [ ] 12:30 Lunch <!--gcal:a-->\n\n## Personal\n",
            ),
            None,
            w(
                "own",
                "daily/2026-10-02.md",
                append_op(
                    planner(),
                    &["- [ ] Buy a card"],
                    Placement::BeforeChildren,
                    false,
                    false,
                ),
            ),
            Outcome::Applied,
            false,
            "## Day planner\n- [ ] Walk\n- [ ] Buy a card\n\n### Calendar\n- [ ] 12:30 Lunch <!--gcal:a-->\n\n## Personal\n",
        ),
        spec(
            "append",
            "journal-paragraph-gets-a-blank-line",
            Some("## Journal\n\nSlept badly.\n\n## Day planner\n"),
            None,
            w(
                "journal",
                "daily/2026-10-02.md",
                append_op(
                    Some(Heading::new("Journal")),
                    &["**21:14** · Late thought."],
                    Placement::End,
                    true,
                    false,
                ),
            ),
            Outcome::Applied,
            false,
            "## Journal\n\nSlept badly.\n\n**21:14** · Late thought.\n\n## Day planner\n",
        ),
        spec(
            "append",
            "first-paragraph-sits-right-under-the-heading",
            Some("## Journal\n\n## Day planner\n"),
            None,
            w(
                "journal2",
                "daily/2026-10-02.md",
                append_op(
                    Some(Heading::new("Journal")),
                    &["**08:10** · First."],
                    Placement::End,
                    true,
                    false,
                ),
            ),
            Outcome::Applied,
            false,
            "## Journal\n**08:10** · First.\n\n## Day planner\n",
        ),
        spec(
            "append",
            "append-of-already-present-lines-is-a-noop",
            Some("## Day planner\n- [ ] Walk\n"),
            None,
            w(
                "noop",
                "daily/2026-10-02.md",
                append_op(planner(), &["- [ ] Walk"], Placement::End, false, false),
            ),
            Outcome::Noop,
            true,
            "## Day planner\n- [ ] Walk\n",
        ),
        spec(
            "append",
            "append-to-end-of-file",
            Some("# Title\n\ntext\n"),
            None,
            w(
                "eof",
                "note.md",
                append_op(None, &["more"], Placement::End, true, false),
            ),
            Outcome::Applied,
            false,
            "# Title\n\ntext\n\nmore\n",
        ),
        spec(
            "append",
            "template-seeds-a-missing-note",
            None,
            Some("# Day\n\n## Journal\n\n___\n\n## Day planner\n"),
            w(
                "seed",
                "daily/2026-10-03.md",
                append_op(planner(), &["- [ ] Dentist"], Placement::End, false, true),
            ),
            Outcome::Created,
            false,
            "# Day\n\n## Journal\n\n___\n\n## Day planner\n- [ ] Dentist\n",
        ),
        spec(
            "append",
            "an-empty-append-only-seeds-a-missing-note",
            None,
            Some("# Day\n\n# Plan\n\n## First\n- [ ] \n\n## Wins\n_Tell me._"),
            w(
                "seed-only",
                "daily/2026-10-03.md",
                append_op(None, &[], Placement::End, false, true),
            ),
            Outcome::Created,
            false,
            "# Day\n\n# Plan\n\n## First\n- [ ] \n\n## Wins\n_Tell me._",
        ),
        spec(
            "append",
            "an-empty-append-leaves-an-existing-note-alone",
            Some("# Day\n\n## Day planner\n- [x] Walk\n"),
            Some("# Day\n\n## Day planner\n"),
            w(
                "seed-only-existing",
                "daily/2026-10-03.md",
                append_op(None, &[], Placement::End, false, true),
            ),
            Outcome::Noop,
            true,
            "# Day\n\n## Day planner\n- [x] Walk\n",
        ),
        spec(
            "replace_line",
            "a-line-with-no-words-is-never-the-target",
            Some("## Day planner\n\n- [ ] \n- [ ] Walk\n"),
            None,
            w(
                "wordless",
                "daily/2026-10-02.md",
                Operation::ReplaceLine {
                    heading: planner(),
                    line_hash: line_hash("- [ ] "),
                    ordinal: 0,
                    new_line: "- [ ] Dentist".into(),
                },
            ),
            Outcome::KeptBoth,
            false,
            "## Day planner\n\n- [ ] \n- [ ] Walk\n- [ ] Dentist <!--thock:also-->\n",
        ),
        spec(
            "append",
            "no-template-makes-a-bare-note",
            None,
            None,
            w(
                "bare",
                "daily/2026-10-03.md",
                append_op(planner(), &["- [ ] Dentist"], Placement::End, false, true),
            ),
            Outcome::Created,
            false,
            "## Day planner\n- [ ] Dentist\n\n",
        ),
        spec(
            "headings",
            "missing-section-goes-before-the-agent-heading",
            Some("# Day\n\n## Personal\n\n- Groceries\n\n# Daily Closure\n\nSummary.\n"),
            None,
            w(
                "agent",
                "daily/2026-10-02.md",
                append_op(
                    Some(Heading::new("Asked on the go")),
                    &["An answer."],
                    Placement::End,
                    false,
                    false,
                ),
            ),
            Outcome::SectionAdded,
            false,
            "# Day\n\n## Personal\n\n- Groceries\n\n## Asked on the go\nAn answer.\n\n# Daily Closure\n\nSummary.\n",
        ),
        spec(
            "headings",
            "missing-section-goes-at-the-end-without-agent-headings",
            Some("# Day\n\n## Journal\nfoo\n"),
            None,
            w(
                "tail",
                "daily/2026-10-02.md",
                append_op(
                    Some(Heading::new("Personal")),
                    &["- x"],
                    Placement::End,
                    false,
                    false,
                ),
            ),
            Outcome::SectionAdded,
            false,
            "# Day\n\n## Journal\nfoo\n\n## Personal\n- x\n",
        ),
        spec(
            "headings",
            "decorated-heading-matches-by-key",
            Some("## 📅 **Day planner**:\n- [ ] Walk\n"),
            None,
            w(
                "key",
                "daily/2026-10-02.md",
                append_op(planner(), &["- [ ] Card"], Placement::End, false, false),
            ),
            Outcome::Applied,
            false,
            "## 📅 **Day planner**:\n- [ ] Walk\n- [ ] Card\n",
        ),
        spec(
            "headings",
            "exact-match-beats-key-match",
            Some("## Day-planner\n- a\n## Day planner\n- b\n"),
            None,
            w(
                "exact",
                "daily/2026-10-02.md",
                append_op(planner(), &["- c"], Placement::End, false, false),
            ),
            Outcome::Applied,
            false,
            "## Day-planner\n- a\n## Day planner\n- b\n- c\n",
        ),
        spec(
            "headings",
            "level-is-ignored-when-matching",
            Some("# Day planner\n- a\n"),
            None,
            w(
                "level",
                "daily/2026-10-02.md",
                append_op(planner(), &["- b"], Placement::End, false, false),
            ),
            Outcome::Applied,
            false,
            "# Day planner\n- a\n- b\n",
        ),
        spec(
            "headings",
            "heading-in-a-fence-is-not-a-heading",
            Some("# Notes\n```\n## Day planner\n```\n"),
            None,
            w(
                "fence",
                "daily/2026-10-02.md",
                append_op(planner(), &["- a"], Placement::End, false, false),
            ),
            Outcome::SectionAdded,
            false,
            "# Notes\n```\n## Day planner\n```\n\n## Day planner\n- a\n",
        ),
        spec(
            "headings",
            "yaml-comment-in-front-matter-is-not-a-heading",
            Some("---\n# Day planner\n---\n# Note\n"),
            None,
            w(
                "yaml",
                "daily/2026-10-02.md",
                append_op(planner(), &["- a"], Placement::End, false, false),
            ),
            Outcome::SectionAdded,
            false,
            "---\n# Day planner\n---\n# Note\n\n## Day planner\n- a\n",
        ),
        spec(
            "headings",
            "created-heading-uses-the-requested-level",
            Some("# Note\n"),
            None,
            w(
                "lvl1",
                "daily/2026-10-02.md",
                append_op(
                    Some(Heading {
                        text: "Asked on the go".into(),
                        level: 1,
                        ordinal: 0,
                    }),
                    &["Answer."],
                    Placement::End,
                    false,
                    false,
                ),
            ),
            Outcome::SectionAdded,
            false,
            "# Note\n\n# Asked on the go\nAnswer.\n",
        ),
        spec(
            "remove_line",
            "remove-is-one-line",
            Some("## Day planner\n- [ ] Walk\n- [ ] Card\n\n## Personal\n"),
            None,
            w(
                "rm",
                "daily/2026-10-02.md",
                Operation::RemoveLine {
                    heading: planner(),
                    line_hash: line_hash("Card"),
                    ordinal: 0,
                },
            ),
            Outcome::Applied,
            false,
            "## Day planner\n- [ ] Walk\n\n## Personal\n",
        ),
        spec(
            "remove_line",
            "remove-of-a-missing-line-is-a-noop",
            Some("## Day planner\n- [ ] Walk\n"),
            None,
            w(
                "rm2",
                "daily/2026-10-02.md",
                Operation::RemoveLine {
                    heading: planner(),
                    line_hash: line_hash("Card"),
                    ordinal: 0,
                },
            ),
            Outcome::Noop,
            true,
            "## Day planner\n- [ ] Walk\n",
        ),
        spec(
            "replace_section",
            "fresh-base-replaces-the-body",
            Some("## Goals\n- a\n- b\n\n## Notes\n"),
            None,
            w(
                "rs",
                "weekly/2026-W40.md",
                Operation::ReplaceSection {
                    heading: Heading::new("Goals"),
                    base_hash: section_hash("## Goals\n- a\n- b\n", &Heading::new("Goals"))
                        .unwrap_or_default(),
                    lines: vec!["- a".into(), "- c".into()],
                },
            ),
            Outcome::Applied,
            false,
            "## Goals\n- a\n- c\n\n## Notes\n",
        ),
        spec(
            "replace_section",
            "stale-base-keeps-both",
            Some("## Goals\n- a\n- b\n\n## Notes\n"),
            None,
            w(
                "rs2",
                "weekly/2026-W40.md",
                Operation::ReplaceSection {
                    heading: Heading::new("Goals"),
                    base_hash: "0000000000000000".into(),
                    lines: vec!["- a".into(), "- d".into()],
                },
            ),
            Outcome::KeptBoth,
            false,
            "## Goals\n- a\n- b\n\n- a <!--thock:also-->\n- d\n\n## Notes\n",
        ),
        spec(
            "replace_section",
            "body-excludes-the-closing-rule",
            Some("## Goals\n\n- a\n\n___\n\n## Notes\n"),
            None,
            w(
                "rs3",
                "weekly/2026-W40.md",
                Operation::ReplaceSection {
                    heading: Heading::new("Goals"),
                    base_hash: section_hash(
                        "## Goals\n\n- a\n\n___\n\n## Notes\n",
                        &Heading::new("Goals"),
                    )
                    .unwrap_or_default(),
                    lines: vec!["- b".into()],
                },
            ),
            Outcome::Applied,
            false,
            "## Goals\n- b\n\n___\n\n## Notes\n",
        ),
        spec(
            "create",
            "create-if-missing",
            None,
            None,
            w(
                "c1",
                "inbox/2026-10-02-1358-idea.md",
                Operation::Create {
                    content: "---\nsource: thock-ios\n---\n\n# Idea\n".into(),
                },
            ),
            Outcome::Created,
            false,
            "---\nsource: thock-ios\n---\n\n# Idea\n",
        ),
        spec(
            "create",
            "create-on-a-different-file-appends",
            Some("Already here.\n"),
            None,
            w(
                "c2",
                "inbox/2026-10-02-1358-idea.md",
                Operation::Create {
                    content: "# Idea\nIts text.\n".into(),
                },
            ),
            Outcome::Applied,
            false,
            "Already here.\n\n# Idea\nIts text.\n",
        ),
        spec(
            "create",
            "create-on-the-same-content-is-a-noop",
            Some("# Idea\r\n"),
            None,
            w(
                "c3",
                "inbox/2026-10-02-1358-idea.md",
                Operation::Create {
                    content: "# Idea\n".into(),
                },
            ),
            Outcome::Noop,
            true,
            "# Idea\r\n",
        ),
        spec(
            "line_endings",
            "crlf-is-preserved-and-used",
            Some("## Day planner\r\n- [ ] Walk\r\n"),
            None,
            w(
                "crlf",
                "daily/2026-10-02.md",
                append_op(planner(), &["- [ ] Card"], Placement::End, false, false),
            ),
            Outcome::Applied,
            false,
            "## Day planner\r\n- [ ] Walk\r\n- [ ] Card\r\n",
        ),
        spec(
            "line_endings",
            "missing-final-newline-stays-missing",
            Some("## Day planner\n- [ ] Walk"),
            None,
            w(
                "nofinal",
                "daily/2026-10-02.md",
                append_op(planner(), &["- [ ] Card"], Placement::End, false, false),
            ),
            Outcome::Applied,
            false,
            "## Day planner\n- [ ] Walk\n- [ ] Card",
        ),
        spec(
            "line_endings",
            "mixed-endings-follow-the-majority",
            Some("## A\r\n- x\r\n- y\n"),
            None,
            w(
                "mixed",
                "daily/2026-10-02.md",
                append_op(
                    Some(Heading::new("A")),
                    &["- z"],
                    Placement::End,
                    false,
                    false,
                ),
            ),
            Outcome::Applied,
            false,
            "## A\r\n- x\r\n- y\n- z\r\n",
        ),
        spec(
            "line_endings",
            "replace-keeps-the-lines-ending",
            Some("## A\r\n- [ ] x\r\n"),
            None,
            w(
                "rep",
                "daily/2026-10-02.md",
                Operation::ReplaceLine {
                    heading: Some(Heading::new("A")),
                    line_hash: line_hash("x"),
                    ordinal: 0,
                    new_line: "- [x] x".into(),
                },
            ),
            Outcome::Applied,
            false,
            "## A\r\n- [x] x\r\n",
        ),
    ]
}

fn write_case(spec: &Spec) -> Case {
    let write = spec.write.clone().unwrap_or_else(roundtrip_probe);
    let before = spec.before.as_deref();
    if let Some(text) = before {
        assert_eq!(
            effect_present(text, &write),
            spec.present_before,
            "{}: present before",
            spec.name
        );
    }
    let Applied { text, outcome } = apply(before, &write, spec.seed.as_deref());
    if let Some(expected) = &spec.expected {
        assert_eq!(&text, expected, "{}: text", spec.name);
    }
    assert_eq!(outcome, spec.outcome, "{}: outcome", spec.name);
    assert_eq!(
        effect_present(&text, &write),
        spec.present_after,
        "{}: present after",
        spec.name
    );
    Case {
        name: spec.name.replace('-', " "),
        before: spec.before.clone(),
        seed: spec.seed.clone(),
        write: spec
            .write
            .as_ref()
            .map(|write| serde_json::from_str(&write.to_json()).expect("write json")),
        after: text,
        outcome,
        effect_present_before: spec.present_before,
        effect_present_after: spec.present_after,
    }
}

fn pretty(value: &impl Serialize) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("serialisable");
    text.push('\n');
    text
}

#[test]
fn generate_fixtures() {
    if !writing() {
        return;
    }
    let root = fixtures_root();
    for area in AREAS {
        let dir = root.join(area);
        fs::create_dir_all(&dir).expect("mkdir");
        for entry in fs::read_dir(&dir).expect("readable").flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("gen-") || name.starts_with("hand-") {
                fs::remove_file(entry.path()).expect("remove stale fixture");
            }
        }
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for note in notes() {
        for spec in matrix(&note) {
            let case = write_case(&spec);
            let path = root.join(spec.area).join(format!("gen-{}.json", spec.name));
            fs::write(&path, pretty(&case)).expect("write fixture");
            *counts.entry(spec.area).or_default() += 1;
        }
    }
    for spec in hand_cases() {
        let case = write_case(&spec);
        let path = root
            .join(spec.area)
            .join(format!("hand-{}.json", spec.name));
        fs::write(&path, pretty(&case)).expect("write fixture");
        *counts.entry(spec.area).or_default() += 1;
    }
    eprintln!("fixtures written: {counts:?}");

    let mut hashes = Vec::new();
    for line in [
        "- [ ] 09:30 - 11:00 Deep work",
        "- [x] 09:30 - 11:00 Deep work",
        "- [ ] 09:30–11:00 Deep work",
        "- [ ] 09:30 — 11:00 Deep work",
        "- [ ] 09:30 to 11:00 Deep work",
        "- [ ] 9:30 Deep work",
        "- [ ] 23:00 - 24:00 Deep work",
        "  - [ ] Deep work",
        "* [X] Deep  work  ",
        "+ Deep work",
        "1. Deep work",
        "12) Deep work",
        "Deep work",
        "- [ ] Deep work <!--gcal:aaaaaaaaaaaa-->",
        "- [ ] Deep work <!--gcal:aaaaaaaaaaaa--> <!--thock:also-->",
        "- [ ] 09:30abc glued time",
        "- [ ] 25:00 not a time",
        "- Groceries: eggs, spinach, lemons 🍋",
        "**21:14** · Slept badly, but the walk fixed it",
        "- [ ] Café com Ana às 12:30",
        "- [ ] 日次計画を見直す",
        "- [ ]",
        "<!-- only a comment -->",
        "",
        "1234567890. ten digits is not a marker",
        "| Report | Friday |",
        "> \"The days are long, but the years are short.\"",
    ] {
        hashes.push(HashVector::Line {
            line: line.into(),
            line_hash: line_hash(line),
        });
    }
    for text in [
        "Day planner",
        "📅 **Day planner**:",
        "Day-planner",
        "[Day planner](plan.md)",
        "[[Day planner]]",
        "[[plan|Day planner]]",
        "![img](x.png)",
        "`code [x](y)` and [a](b)",
        "Journal ##",
        "日次計画",
        "Тайлан",
        "***",
        "   Asked on the go   ",
    ] {
        hashes.push(HashVector::Heading {
            text: text.into(),
            heading_key: heading_key(text),
        });
    }
    for body in [vec![], vec!["- a", "- b"], vec!["x", "", "y"]] {
        let note = format!("## S\n{}\n", body.join("\n"));
        hashes.push(HashVector::Section {
            body: body.iter().map(|line| line.to_string()).collect(),
            section_hash: section_hash(&note, &Heading::new("S")).expect("section"),
        });
    }
    fs::write(root.join("hashes.json"), pretty(&hashes)).expect("write hashes");

    let standard = base64::engine::general_purpose::STANDARD;
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = index as u8;
    }
    let nonce: [u8; 12] = [
        0x07, 0, 0, 0, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
    ];
    let mut envelopes = Vec::new();
    for (context, plaintext) in [
        (
            Context::File {
                path: "daily/2026-10-02.md".into(),
                blob_id: "5d2c1f0e9a8b7c6d5e4f3a2b1c0d9e8f".into(),
            },
            b"# Thursday\n\n## Journal\n\nSlept badly.\n".to_vec(),
        ),
        (
            Context::File {
                path: "reference/clips/caf\u{e9}.md".into(),
                blob_id: "00000000000000000000000000000001".into(),
            },
            Vec::new(),
        ),
        (
            Context::Write {
                client_id: "0f7e0b1a-3c4d-4e5f-8a9b-0c1d2e3f4a5b".into(),
            },
            br#"{"v":1,"client_id":"0f7e0b1a-3c4d-4e5f-8a9b-0c1d2e3f4a5b","path":"daily/2026-10-02.md","made_at":"2026-10-02T13:58:02Z","device_id":"c41a2f8e9b7d1c35","kind":"append","heading":{"text":"Day planner","level":2,"ordinal":0},"lines":["- [ ] Buy a card"],"placement":"end","blank_line_before":false,"create_from_template":false}"#.to_vec(),
        ),
    ] {
        let envelope = seal_with_nonce(&key, &nonce, context.clone(), &plaintext);
        let context_value = match &context {
            Context::File { path, blob_id } => serde_json::json!({"kind": "file", "path": path, "blob_id": blob_id}),
            Context::Write { client_id } => serde_json::json!({"kind": "write", "client_id": client_id}),
        };
        envelopes.push(EnvelopeVector::Sealed {
            key: hex::encode(key),
            nonce: hex::encode(nonce),
            context: context_value,
            plaintext: standard.encode(&plaintext),
            envelope: standard.encode(&envelope),
            content_hash: content_hash(&envelope),
        });
    }
    envelopes.push(EnvelopeVector::KeyCheck {
        key: hex::encode(key),
        key_check: key_check(&key),
    });
    fs::write(root.join("envelope.json"), pretty(&envelopes)).expect("write envelopes");
}
