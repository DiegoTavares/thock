//! The part of vault sync (spec `thock/specs/v34-vault-sync-api.md`) that must
//! behave identically on the desk and on the phone: how a write document is
//! read, how it finds its target in a note, how it is applied, how lines and
//! sections are hashed, and how blobs are sealed. No I/O, no async, no UI, so
//! the same code compiles for iOS.

mod apply;
mod envelope;
mod markdown;
mod write;

pub use apply::{Applied, Outcome, apply, effect_present};
pub use envelope::{Context, SealError, content_hash, key_check, open, seal, seal_with_nonce};
pub use markdown::{heading_key, line_hash, section_hash};
pub use write::{Heading, Operation, Placement, Write, WriteError, parse_write};

use unicode_normalization::is_nfc;

/// Extensions that sync (spec §4.1), compared case-insensitively.
pub const SYNCABLE_EXTENSIONS: [&str; 5] = ["md", "txt", "toml", "json", "csv"];
/// Folders whose contents never sync, whatever their extension.
pub const EXCLUDED_PREFIXES: [&str; 4] =
    [".thock/history/", ".thock/cache/", ".thock/sync/", ".git/"];
/// Longest path the server accepts, in bytes of UTF-8.
pub const MAX_PATH_BYTES: usize = 1024;

/// Whether `path` is one the desk may upload and the phone may write to
/// (spec §4.1): relative, `/`-separated, NFC, allow-listed extension, and not
/// under a folder that sync ignores.
pub fn is_syncable_path(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || !is_nfc(path) {
        return false;
    }
    if path.starts_with('/') || path.ends_with('/') || path.contains('\\') {
        return false;
    }
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return false;
    }
    if path.chars().any(char::is_control) {
        return false;
    }
    if EXCLUDED_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return false;
    }
    let Some((stem, extension)) = path.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || stem.ends_with('/') {
        return false;
    }
    SYNCABLE_EXTENSIONS
        .iter()
        .any(|allowed| extension.eq_ignore_ascii_case(allowed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syncable_paths() {
        for path in [
            "daily/2026-10-02.md",
            "backlog.md",
            ".thock/config.toml",
            "routines/inbox/routine.toml",
            "reference/clips/A Title.MD",
            "data.csv",
            "notes/caf\u{e9}.md",
        ] {
            assert!(is_syncable_path(path), "{path}");
        }
    }

    #[test]
    fn unsyncable_paths() {
        let long = format!("{}.md", "a".repeat(MAX_PATH_BYTES));
        for path in [
            "",
            "/daily/x.md",
            "daily/x.md/",
            "daily//x.md",
            "./x.md",
            "../x.md",
            "daily/../x.md",
            "image.png",
            "notes/.md",
            "noext",
            ".thock/history/HEAD",
            ".thock/cache/index.json",
            ".thock/sync/state.json",
            ".git/config",
            "a\\b.md",
            "bad\u{7}.md",
            "notes/cafe\u{301}.md",
            long.as_str(),
        ] {
            assert!(!is_syncable_path(path), "{path}");
        }
    }
}
