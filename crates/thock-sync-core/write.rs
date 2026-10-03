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
}

impl Operation {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Append { .. } => "append",
            Self::ReplaceLine { .. } => "replace_line",
            Self::RemoveLine { .. } => "remove_line",
            Self::ReplaceSection { .. } => "replace_section",
        }
    }

    pub fn heading(&self) -> Option<&Heading> {
        match self {
            Self::Create { .. } => None,
            Self::Append { heading, .. }
            | Self::ReplaceLine { heading, .. }
            | Self::RemoveLine { heading, .. } => heading.as_ref(),
            Self::ReplaceSection { heading, .. } => Some(heading),
        }
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
    #[error("the write has no client id")]
    MissingClientId,
    #[error("the write names no path")]
    MissingPath,
    #[error("a line in the write contains a line break")]
    LineBreakInLine,
    #[error("the heading text is empty")]
    EmptyHeading,
}

/// Parses and validates a write document (spec §7). Unknown fields are
/// ignored so a newer phone can talk to an older desk.
pub fn parse_write(json: &str) -> Result<Write, WriteError> {
    let write: Write =
        serde_json::from_str(json).map_err(|error| WriteError::Json(error.to_string()))?;
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
    if let Some(heading) = write.operation.heading()
        && heading.text.trim().is_empty()
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
        Operation::RemoveLine { .. } => false,
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
