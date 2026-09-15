//! The agent's memory of the person (V28): a `memory/` folder of plain
//! Markdown the Reflect ritual writes and every session reads. This module
//! owns the scaffold, the capped read of `memory/index.md` that goes into the
//! hosted session prompt, and the chat panel's "Reflect now?" nudge counter.
//! Nothing here calls a model; the writing is the ritual's job.

use anyhow::{Context as _, Result};
use std::fs;
use std::path::{Path, PathBuf};
use util::ResultExt as _;

use crate::vault::{VAULT_MARKER_DIR, write_if_missing};

pub const MEMORY_DIR: &str = "memory";
pub const INDEX_PATH: &str = "memory/index.md";
pub const INBOX_PATH: &str = "memory/inbox.md";
pub const REFLECT_SKILL_PATH: &str = "skills/thock/reflect.md";
pub const REBUILD_MEMORY_SKILL_PATH: &str = "skills/thock/rebuild-memory.md";

const INDEX_SEED: &str = include_str!("../assets/memory/index.md");
const INBOX_SEED: &str = include_str!("../assets/memory/inbox.md");
pub(crate) const REFLECT_SKILL: &str = include_str!("../assets/skills/reflect.md");
pub(crate) const REBUILD_MEMORY_SKILL: &str = include_str!("../assets/skills/rebuild-memory.md");

/// Writes the memory folder, create-if-missing like the rest of the vault
/// scaffold: its pages are the person's once written. The two rituals that
/// maintain it are shipped core files (`routines::shipped_core_files`), so
/// they upgrade with the app (V29). A vault that predates V28 gains all of
/// it on its next reconcile pass.
pub fn materialize(vault_root: &Path) -> Result<()> {
    write_if_missing(&vault_root.join(INDEX_PATH), INDEX_SEED)?;
    write_if_missing(&vault_root.join(INBOX_PATH), INBOX_SEED)?;
    Ok(())
}

/// The line the truncated index ends with, so the agent knows the page is
/// over its cap and the next Reflect will trim it.
pub const INDEX_OVER_CAP_LINE: &str =
    "(The rest of this page is over its cap; the next Reflect will trim it.)";

/// `memory/index.md` as the session prompt carries it: the whole file when
/// it fits in `index_lines`, else the first `index_lines` lines and a note
/// that it was cut. `None` when there is no index or it says nothing yet, so
/// the prompt never carries an empty block. Blocking I/O.
pub fn read_index_capped(vault_root: &Path, index_lines: usize) -> Option<String> {
    let raw = match fs::read_to_string(vault_root.join(INDEX_PATH)) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            log::warn!("Thock: couldn't read {INDEX_PATH}: {error}");
            return None;
        }
    };
    cap_index(&raw, index_lines)
}

/// The pure half of [`read_index_capped`].
pub fn cap_index(raw: &str, index_lines: usize) -> Option<String> {
    if !has_entries(raw) {
        return None;
    }
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() <= index_lines {
        return Some(raw.trim_end().to_string());
    }
    let mut kept = lines[..index_lines].join("\n");
    kept.push('\n');
    kept.push_str(INDEX_OVER_CAP_LINE);
    Some(kept)
}

/// Whether a memory page says anything: at least one list item. Headings,
/// the scaffolded explainer and blank lines don't count, so a fresh vault
/// reads as empty.
pub fn has_entries(raw: &str) -> bool {
    raw.lines().any(|line| {
        let line = line.trim_start();
        (line.starts_with("- ") || line.starts_with("* ")) && line.len() > 2
    })
}

/// Whether `memory/inbox.md` holds facts the Reflect ritual hasn't filed
/// yet. A missing inbox is an empty one. Blocking I/O.
pub fn inbox_has_entries(vault_root: &Path) -> bool {
    match fs::read_to_string(vault_root.join(INBOX_PATH)) {
        Ok(raw) => has_entries(&raw),
        Err(_) => false,
    }
}

fn nudge_counter_path(vault_root: &Path) -> PathBuf {
    vault_root
        .join(VAULT_MARKER_DIR)
        .join("state")
        .join("memory")
        .join("sessions-with-inbox")
}

fn read_nudge_counter(vault_root: &Path) -> usize {
    fs::read_to_string(nudge_counter_path(vault_root))
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(0)
}

fn write_nudge_counter(vault_root: &Path, count: usize) -> Result<()> {
    let path = nudge_counter_path(vault_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&path, count.to_string()).with_context(|| format!("writing {}", path.display()))
}

/// Records that a chat session started and says whether the panel should
/// suggest Reflect: true once `nudge_after_sessions` sessions have begun
/// with an unemptied inbox. An empty inbox (Reflect ran, or nothing was
/// ever noted) resets the count. A `nudge_after_sessions` of zero turns the
/// nudge off. Blocking I/O — call from a background thread.
pub fn note_session_started(vault_root: &Path, nudge_after_sessions: usize) -> bool {
    if nudge_after_sessions == 0 || !inbox_has_entries(vault_root) {
        if read_nudge_counter(vault_root) != 0 {
            write_nudge_counter(vault_root, 0).log_err();
        }
        return false;
    }
    let count = read_nudge_counter(vault_root).saturating_add(1);
    write_nudge_counter(vault_root, count).log_err();
    count >= nudge_after_sessions
}

/// The person said "not now": start counting again from zero.
pub fn dismiss_nudge(vault_root: &Path) -> Result<()> {
    write_nudge_counter(vault_root, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_creates_the_memory_files_once() {
        let dir = tempfile::tempdir().unwrap();
        materialize(dir.path()).unwrap();
        for path in [INDEX_PATH, INBOX_PATH] {
            assert!(dir.path().join(path).is_file(), "{path} missing");
        }
        let index = dir.path().join(INDEX_PATH);
        fs::write(&index, "# Mine\n\n- Kept.\n").unwrap();
        materialize(dir.path()).unwrap();
        assert_eq!(fs::read_to_string(&index).unwrap(), "# Mine\n\n- Kept.\n");
    }

    #[test]
    fn a_fresh_index_reads_as_nothing_known() {
        assert!(!has_entries(INDEX_SEED));
        assert!(!has_entries(INBOX_SEED));
        assert_eq!(cap_index(INDEX_SEED, 120), None);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_index_capped(dir.path(), 120), None);
    }

    #[test]
    fn an_index_under_the_cap_is_carried_whole() {
        let raw = "# What Thock has learned\n\n## People\n- **Ana**, your manager.\n";
        assert_eq!(
            cap_index(raw, 120).as_deref(),
            Some("# What Thock has learned\n\n## People\n- **Ana**, your manager.")
        );
    }

    #[test]
    fn an_index_over_the_cap_is_cut_and_says_so() {
        let raw = "# Learned\n- one\n- two\n- three\n- four\n";
        let capped = cap_index(raw, 3).unwrap();
        assert_eq!(
            capped,
            format!("# Learned\n- one\n- two\n{INDEX_OVER_CAP_LINE}")
        );
        assert!(!capped.contains("- three"));
    }

    #[test]
    fn the_nudge_counts_sessions_with_an_unemptied_inbox() {
        let dir = tempfile::tempdir().unwrap();
        materialize(dir.path()).unwrap();
        assert!(!note_session_started(dir.path(), 2));
        assert!(!inbox_has_entries(dir.path()));

        fs::write(
            dir.path().join(INBOX_PATH),
            "# Noted\n\n- 2026-09-15 · Rui invoices monthly.\n",
        )
        .unwrap();
        assert!(inbox_has_entries(dir.path()));
        assert!(!note_session_started(dir.path(), 2));
        assert!(note_session_started(dir.path(), 2));
        assert!(note_session_started(dir.path(), 2));

        dismiss_nudge(dir.path()).unwrap();
        assert!(!note_session_started(dir.path(), 2));

        // Reflect emptied the inbox: the count starts over.
        fs::write(dir.path().join(INBOX_PATH), INBOX_SEED).unwrap();
        assert!(!note_session_started(dir.path(), 2));
        assert_eq!(read_nudge_counter(dir.path()), 0);

        // A zero threshold never nudges.
        fs::write(dir.path().join(INBOX_PATH), "- a fact\n").unwrap();
        assert!(!note_session_started(dir.path(), 0));
    }
}
