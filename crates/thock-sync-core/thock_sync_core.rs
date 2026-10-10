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
pub use write::{Heading, Operation, Place, Placement, Write, WriteError, parse_write};

use unicode_normalization::is_nfc;

/// Extensions that sync (spec §4.1), compared case-insensitively.
pub const SYNCABLE_EXTENSIONS: [&str; 5] = ["md", "txt", "toml", "json", "csv"];
/// The picture formats that sync as binary snapshots, from the vault's
/// images folder only (V39 §6.1).
pub const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];
/// Folders whose contents never sync, whatever their extension.
pub const EXCLUDED_PREFIXES: [&str; 4] =
    [".thock/history/", ".thock/cache/", ".thock/sync/", ".git/"];
/// Longest path the server accepts, in bytes of UTF-8.
pub const MAX_PATH_BYTES: usize = 1024;

/// Whether `path` is one the desk may upload and the phone may write to
/// (spec §4.1): relative, `/`-separated, NFC, allow-listed extension, and not
/// under a folder that sync ignores.
pub fn is_syncable_path(path: &str) -> bool {
    well_formed_extension(path).is_some_and(|extension| {
        SYNCABLE_EXTENSIONS
            .iter()
            .any(|allowed| extension.eq_ignore_ascii_case(allowed))
    })
}

/// Whether `path` is a picture that syncs as bytes (V39 §6.1): the same
/// path rules, an image extension, and the first segment is the vault's
/// images folder. Folder and extension both, so the text boundary V34 drew
/// stays explainable in one sentence.
pub fn is_syncable_image_path(path: &str, images_dir: &str) -> bool {
    let images_dir = images_dir.trim_matches('/');
    if images_dir.is_empty() {
        return false;
    }
    let Some(extension) = well_formed_extension(path) else {
        return false;
    };
    path.strip_prefix(images_dir)
        .is_some_and(|rest| rest.starts_with('/'))
        && IMAGE_EXTENSIONS
            .iter()
            .any(|allowed| extension.eq_ignore_ascii_case(allowed))
}

/// The extension of a path that passes every rule of spec §4.1 but the
/// allow-list, or `None`.
fn well_formed_extension(path: &str) -> Option<&str> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || !is_nfc(path) {
        return None;
    }
    if path.starts_with('/') || path.ends_with('/') || path.contains('\\') {
        return None;
    }
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return None;
    }
    if path.chars().any(char::is_control) {
        return None;
    }
    if EXCLUDED_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return None;
    }
    let (stem, extension) = path.rsplit_once('.')?;
    if stem.is_empty() || stem.ends_with('/') {
        return None;
    }
    Some(extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_sync_from_the_images_folder_only() {
        for path in [
            "images/2026-10-10-0931-whiteboard.jpg",
            "images/receipt.PNG",
            "images/a b.webp",
            "images/nested/cover.gif",
        ] {
            assert!(is_syncable_image_path(path, "images"), "{path}");
            assert!(!is_syncable_path(path), "{path} is not text");
        }
        for path in [
            "photo.png",
            "daily/photo.png",
            "images.png",
            "imagesx/photo.png",
            "images/note.md",
            "images/scan.pdf",
            "images/.png",
            "images/../photo.png",
            "/images/photo.png",
        ] {
            assert!(!is_syncable_image_path(path, "images"), "{path}");
        }
        assert!(is_syncable_image_path("pictures/a.png", "pictures"));
        assert!(!is_syncable_image_path("images/a.png", "pictures"));
        assert!(!is_syncable_image_path("images/a.png", ""));
    }

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
