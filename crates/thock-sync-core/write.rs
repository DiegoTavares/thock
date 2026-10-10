//! The write document (spec §7): what the phone encrypts into a write
//! payload and what the desk applies.

use serde::{Deserialize, Serialize};

/// The document format this crate reads and writes.
pub const WRITE_VERSION: u32 = 1;

/// The heading a write targets (spec §7.2). `text` is matched by V26's
/// heading key; `level` is used only when the heading must be created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heading {
    pub text: String,
    #[serde(default = "default_level")]
    pub level: u8,
    #[serde(default)]
    pub ordinal: usize,
}

impl Heading {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            level: default_level(),
            ordinal: 0,
        }
    }

    /// The line that creates this heading when a note lacks it.
    pub(crate) fn line(&self) -> String {
        let level = self.level.clamp(1, 6) as usize;
        format!("{} {}", "#".repeat(level), self.text.trim())
    }
}

fn default_level() -> u8 {
    2
}

/// Where an `append` lands inside its section (spec §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// After the body's last line, below any subsections.
    #[default]
    End,
    /// After the section's own lines, above its first subsection.
    BeforeChildren,
}

/// Where a `move_block` lands inside its destination group (V38 §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    /// Before the group's first body line.
    Top,
    /// After the group's own lines, above its first subsection.
    #[default]
    End,
    /// After the named line and its indented continuation.
    After {
        line_hash: String,
        #[serde(default)]
        ordinal: usize,
    },
}

/// The kind-specific half of a write (spec §7.3), tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Operation {
    Create {
        content: String,
    },
    Append {
        heading: Option<Heading>,
        lines: Vec<String>,
        #[serde(default)]
        placement: Placement,
        #[serde(default)]
        blank_line_before: bool,
        #[serde(default)]
        create_from_template: bool,
    },
    ReplaceLine {
        heading: Option<Heading>,
        line_hash: String,
        #[serde(default)]
        ordinal: usize,
        new_line: String,
    },
    RemoveLine {
        heading: Option<Heading>,
        line_hash: String,
        #[serde(default)]
        ordinal: usize,
    },
    ReplaceSection {
        heading: Heading,
        base_hash: String,
        lines: Vec<String>,
    },
    /// Moves a task block, the line plus its indented continuation, from
    /// the group under `heading` to the group under `to` (V38 §7.1).
    MoveBlock {
        heading: Option<Heading>,
        line_hash: String,
        #[serde(default)]
        ordinal: usize,
        to: Option<Heading>,
        #[serde(default)]
        place: Place,
        /// Replaces the block's first line as it lands.
        #[serde(default)]
        new_line: Option<String>,
        /// The section a missing `to` is created at the end of.
        #[serde(default)]
        create_under: Option<Heading>,
    },
    RemoveBlock {
        heading: Option<Heading>,
        line_hash: String,
        #[serde(default)]
        ordinal: usize,
    },
}

/// Every `kind` this build applies. A write naming another kind comes from a
/// newer phone and must wait for a newer desk, not be dropped.
pub const KNOWN_KINDS: [&str; 7] = [
    "create",
    "append",
    "replace_line",
    "remove_line",
    "replace_section",
    "move_block",
    "remove_block",
];

impl Operation {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Append { .. } => "append",
            Self::ReplaceLine { .. } => "replace_line",
            Self::RemoveLine { .. } => "remove_line",
            Self::ReplaceSection { .. } => "replace_section",
            Self::MoveBlock { .. } => "move_block",
            Self::RemoveBlock { .. } => "remove_block",
        }
    }

    pub fn heading(&self) -> Option<&Heading> {
        match self {
            Self::Create { .. } => None,
            Self::Append { heading, .. }
            | Self::ReplaceLine { heading, .. }
            | Self::RemoveLine { heading, .. }
            | Self::MoveBlock { heading, .. }
            | Self::RemoveBlock { heading, .. } => heading.as_ref(),
            Self::ReplaceSection { heading, .. } => Some(heading),
        }
    }

    /// Every heading the write names, for validation.
    fn headings(&self) -> Vec<&Heading> {
        let mut named: Vec<&Heading> = self.heading().into_iter().collect();
        if let Self::MoveBlock {
            to, create_under, ..
        } = self
        {
            named.extend(to.iter());
            named.extend(create_under.iter());
        }
        named
    }
}

/// One write from the phone (spec §7.1). Serialises to the canonical form
/// the `roundtrip` fixtures pin: `v`, `client_id`, `path`, `made_at`,
/// `device_id`, then `kind` and its fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Write {
    pub v: u32,
    pub client_id: String,
    pub path: String,
    #[serde(default)]
    pub made_at: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(flatten)]
    pub operation: Operation,
}

impl Write {
    pub fn new(
        client_id: impl Into<String>,
        path: impl Into<String>,
        operation: Operation,
    ) -> Self {
        Self {
            v: WRITE_VERSION,
            client_id: client_id.into(),
            path: path.into(),
            made_at: String::new(),
            device_id: String::new(),
            operation,
        }
    }

    /// The canonical JSON of this write, the bytes a write envelope seals.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WriteError {
    #[error("the write isn't valid JSON: {0}")]
    Json(String),
    #[error("the write is version {0}; this build reads version {WRITE_VERSION}")]
    UnsupportedVersion(u32),
    #[error("the write is a `{0}`, which this build doesn't know")]
    UnsupportedKind(String),
    #[error("the write has no client id")]
    MissingClientId,
    #[error("the write names no path")]
    MissingPath,
    #[error("a line in the write contains a line break")]
    LineBreakInLine,
    #[error("the heading text is empty")]
    EmptyHeading,
}

impl WriteError {
    /// The write is well formed but from a newer Thock than this build: a
    /// reader must hold it rather than discard it (spec §10.4).
    pub fn needs_newer_reader(&self) -> bool {
        match self {
            // An older version than this build ever read is garbage: no update
            // would make it readable, so holding the queue on it never clears.
            Self::UnsupportedVersion(version) => *version > WRITE_VERSION,
            Self::UnsupportedKind(_) => true,
            _ => false,
        }
    }
}

/// Parses and validates a write document (spec §7). Unknown fields are
/// ignored so a newer phone can talk to an older desk; an unknown `kind` or
/// `v` is reported as such, so the desk can tell "update me" from garbage.
pub fn parse_write(json: &str) -> Result<Write, WriteError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| WriteError::Json(error.to_string()))?;
    if let Some(version) = value.get("v").and_then(serde_json::Value::as_u64)
        && version != u64::from(WRITE_VERSION)
    {
        return Err(WriteError::UnsupportedVersion(
            u32::try_from(version).unwrap_or(u32::MAX),
        ));
    }
    if let Some(kind) = value.get("kind").and_then(serde_json::Value::as_str)
        && !KNOWN_KINDS.contains(&kind)
    {
        return Err(WriteError::UnsupportedKind(kind.to_string()));
    }
    let write: Write =
        serde_json::from_value(value).map_err(|error| WriteError::Json(error.to_string()))?;
    validate(&write)?;
    Ok(write)
}

pub(crate) fn validate(write: &Write) -> Result<(), WriteError> {
    if write.v != WRITE_VERSION {
        return Err(WriteError::UnsupportedVersion(write.v));
    }
    if write.client_id.trim().is_empty() {
        return Err(WriteError::MissingClientId);
    }
    if write.path.trim().is_empty() {
        return Err(WriteError::MissingPath);
    }
    if write
        .operation
        .headings()
        .iter()
        .any(|heading| heading.text.trim().is_empty())
    {
        return Err(WriteError::EmptyHeading);
    }
    let has_break = |line: &String| line.contains('\n') || line.contains('\r');
    let broken = match &write.operation {
        Operation::Create { .. } => false,
        Operation::Append { lines, .. } | Operation::ReplaceSection { lines, .. } => {
            lines.iter().any(has_break)
        }
        Operation::ReplaceLine { new_line, .. } => has_break(new_line),
        Operation::MoveBlock { new_line, .. } => new_line.as_ref().is_some_and(has_break),
        Operation::RemoveLine { .. } | Operation::RemoveBlock { .. } => false,
    };
    if broken {
        return Err(WriteError::LineBreakInLine);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_spec_example() {
        let json = r#"{"v":1,"client_id":"0f7e0b1a","kind":"replace_line","path":"daily/2026-10-02.md","made_at":"2026-10-02T13:58:02Z","device_id":"c41a","heading":{"text":"Day planner","level":2,"ordinal":0},"line_hash":"abc","ordinal":0,"new_line":"- [x] Deep work","future":true}"#;
        let write = parse_write(json).expect("parses");
        assert_eq!(write.path, "daily/2026-10-02.md");
        match &write.operation {
            Operation::ReplaceLine {
                heading, new_line, ..
            } => {
                assert_eq!(
                    heading.as_ref().map(|h| h.text.as_str()),
                    Some("Day planner")
                );
                assert_eq!(new_line, "- [x] Deep work");
            }
            other => panic!("wrong kind {}", other.kind()),
        }
    }

    #[test]
    fn defaults_apply() {
        let json = r#"{"v":1,"client_id":"c","kind":"append","path":"p.md","heading":{"text":"Journal"},"lines":["x"]}"#;
        let write = parse_write(json).expect("parses");
        match write.operation {
            Operation::Append {
                heading,
                placement,
                blank_line_before,
                create_from_template,
                ..
            } => {
                let heading = heading.expect("heading");
                assert_eq!(heading.level, 2);
                assert_eq!(heading.ordinal, 0);
                assert_eq!(placement, Placement::End);
                assert!(!blank_line_before);
                assert!(!create_from_template);
            }
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn rejects_bad_documents() {
        assert!(matches!(parse_write("{"), Err(WriteError::Json(_))));
        assert_eq!(
            parse_write(r#"{"v":2,"client_id":"c","kind":"create","path":"p.md","content":""}"#),
            Err(WriteError::UnsupportedVersion(2))
        );
        assert_eq!(
            parse_write(r#"{"v":1,"client_id":"","kind":"create","path":"p.md","content":""}"#),
            Err(WriteError::MissingClientId)
        );
        assert_eq!(
            parse_write(
                r#"{"v":1,"client_id":"c","kind":"append","path":"p.md","heading":null,"lines":["a\nb"]}"#
            ),
            Err(WriteError::LineBreakInLine)
        );
        assert_eq!(
            parse_write(
                r#"{"v":1,"client_id":"c","kind":"replace_section","heading":{"text":" "},"base_hash":"x","lines":[],"path":"p.md"}"#
            ),
            Err(WriteError::EmptyHeading)
        );
        assert!(matches!(
            parse_write(
                r#"{"v":1,"client_id":"c","kind":"replace_section","heading":null,"base_hash":"x","lines":[],"path":"p.md"}"#
            ),
            Err(WriteError::Json(_))
        ));
    }

    #[test]
    fn a_newer_phones_write_is_unsupported_not_garbage() {
        let future_kind = r#"{"v":1,"client_id":"c","kind":"swap_blocks","path":"backlog.md","first":"a","second":"b"}"#;
        let error = parse_write(future_kind).expect_err("unknown kind");
        assert_eq!(error, WriteError::UnsupportedKind("swap_blocks".into()));
        assert!(error.needs_newer_reader());

        let future_version = r#"{"v":2,"client_id":"c","kind":"teleport","path":"p.md"}"#;
        let error = parse_write(future_version).expect_err("unknown version");
        assert_eq!(error, WriteError::UnsupportedVersion(2));
        assert!(error.needs_newer_reader());

        let older_version = r#"{"v":0,"client_id":"c","kind":"create","path":"p.md","content":""}"#;
        let error = parse_write(older_version).expect_err("older version");
        assert_eq!(error, WriteError::UnsupportedVersion(0));
        assert!(
            !error.needs_newer_reader(),
            "no update can read a version 0"
        );

        for garbage in [
            r#"{"v":1,"client_id":"c","kind":7,"path":"p.md"}"#,
            r#"{"v":"1","client_id":"c","kind":"create","path":"p.md","content":""}"#,
            r#"{"v":1,"client_id":"c","path":"p.md"}"#,
        ] {
            let error = parse_write(garbage).expect_err(garbage);
            assert!(matches!(error, WriteError::Json(_)), "{garbage}: {error}");
            assert!(!error.needs_newer_reader());
        }
        assert!(!WriteError::MissingPath.needs_newer_reader());
    }

    #[test]
    fn parses_a_move_block() {
        let json = r#"{"v":1,"client_id":"c","kind":"move_block","path":"backlog.md","heading":{"text":"Home","level":3},"line_hash":"abc","to":{"text":"Someday"},"place":{"after":{"line_hash":"def"}},"create_under":{"text":"Someday"}}"#;
        let write = parse_write(json).expect("parses");
        match &write.operation {
            Operation::MoveBlock {
                to,
                place,
                new_line,
                ..
            } => {
                assert_eq!(to.as_ref().map(|h| h.text.as_str()), Some("Someday"));
                assert_eq!(
                    *place,
                    Place::After {
                        line_hash: "def".into(),
                        ordinal: 0
                    }
                );
                assert_eq!(*new_line, None);
            }
            other => panic!("wrong kind {}", other.kind()),
        }
        assert_eq!(parse_write(&write.to_json()).expect("parses"), write);
        let bare = r#"{"v":1,"client_id":"c","kind":"move_block","path":"backlog.md","heading":null,"line_hash":"abc","to":null}"#;
        match parse_write(bare).expect("parses").operation {
            Operation::MoveBlock { place, .. } => assert_eq!(place, Place::End),
            _ => panic!("wrong kind"),
        }
        assert_eq!(
            parse_write(
                r#"{"v":1,"client_id":"c","kind":"move_block","path":"backlog.md","heading":null,"line_hash":"abc","to":{"text":" "}}"#
            ),
            Err(WriteError::EmptyHeading)
        );
        assert_eq!(
            parse_write(
                r#"{"v":1,"client_id":"c","kind":"move_block","path":"backlog.md","heading":null,"line_hash":"abc","to":null,"new_line":"a\nb"}"#
            ),
            Err(WriteError::LineBreakInLine)
        );
    }

    #[test]
    fn canonical_json_round_trips() {
        let write = Write::new(
            "c",
            "p.md",
            Operation::Append {
                heading: None,
                lines: vec!["a".into()],
                placement: Placement::BeforeChildren,
                blank_line_before: true,
                create_from_template: false,
            },
        );
        let json = write.to_json();
        assert_eq!(
            json,
            r#"{"v":1,"client_id":"c","path":"p.md","made_at":"","device_id":"","kind":"append","heading":null,"lines":["a"],"placement":"before_children","blank_line_before":true,"create_from_template":false}"#
        );
        assert_eq!(parse_write(&json).expect("parses"), write);
    }
}
