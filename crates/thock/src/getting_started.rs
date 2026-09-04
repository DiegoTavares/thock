//! First-run Getting started state (V18 §5.4).
//!
//! The checklist's state is plain marker files under
//! `.thock/state/getting-started/` — never `config.toml`, whose
//! `deny_unknown_fields` schema would make older builds treat the whole
//! vault as invalid (V7 §9 trap 4). Two of the three steps need no stored
//! state at all: agent connection is read live from the resolved command,
//! and the tour completes through the standard V5 done-marker protocol.

use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

use crate::vault::{VAULT_MARKER_DIR, write_if_missing};

/// The Introductory Guide, materialized into every vault create-if-missing
/// (V18 §5.3) and opened with the system browser like other browser links.
pub const GUIDE_PATH: &str = "guide/index.html";
pub(crate) const GUIDE_HTML: &str = include_str!("../assets/guide/index.html");

/// The customize page — themes, text size, and the keys worth knowing — a
/// vault note opened as a rendered preview by the checklist's "Customize"
/// step (which also pops the live theme selector on top).
pub const CUSTOMIZE_PATH: &str = "guide/customize.md";
pub(crate) const CUSTOMIZE_NOTE: &str = include_str!("../assets/guide/customize.md");

/// The Welcome Tour ritual (V18 §5.5) and the id of its done marker under
/// `.thock/state/onboarded/` (the V5 §5.4 protocol).
pub const WELCOME_TOUR_SKILL_PATH: &str = "skills/thock/welcome-tour.md";
pub(crate) const WELCOME_TOUR_SKILL: &str = include_str!("../assets/skills/welcome-tour.md");
pub const WELCOME_TOUR_MARKER_ID: &str = "welcome-tour";

fn state_dir(vault_root: &Path) -> PathBuf {
    vault_root
        .join(VAULT_MARKER_DIR)
        .join("state")
        .join("getting-started")
}

fn active_marker(vault_root: &Path) -> PathBuf {
    state_dir(vault_root).join("active")
}

fn introduction_marker(vault_root: &Path) -> PathBuf {
    state_dir(vault_root).join("introduction")
}

fn customize_marker(vault_root: &Path) -> PathBuf {
    state_dir(vault_root).join("customize")
}

/// Turns the checklist on. Called only when a scaffold *creates* the vault —
/// which is what "fresh vaults only" (V18 decision 3) means mechanically:
/// an existing vault never gains the marker, so it never sees the section.
pub fn activate(vault_root: &Path) -> Result<()> {
    write_if_missing(&active_marker(vault_root), "")
}

/// The four steps, each derived from evidence rather than a click: the
/// guide was opened, the customize page was opened, a launch command
/// resolves, the tour wrote its done marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Steps {
    pub introduction: bool,
    pub customize: bool,
    pub agent: bool,
    pub tour: bool,
}

impl Steps {
    pub fn all_done(self) -> bool {
        self.introduction && self.customize && self.agent && self.tour
    }
}

/// The checklist to render, or `None` while inactive. `agent_connected` is
/// passed in because resolving the command reads the global settings file —
/// callers already do that off the UI thread.
pub fn state(vault_root: &Path, agent_connected: bool) -> Option<Steps> {
    if !active_marker(vault_root).exists() {
        return None;
    }
    Some(Steps {
        introduction: introduction_marker(vault_root).exists(),
        customize: customize_marker(vault_root).exists(),
        agent: agent_connected,
        tour: crate::routines::onboarding_marker_path(vault_root, WELCOME_TOUR_MARKER_ID).exists(),
    })
}

/// Records that the guide was opened from the checklist.
pub fn mark_introduction_read(vault_root: &Path) -> Result<()> {
    write_if_missing(&introduction_marker(vault_root), "")
}

/// Records that the customize page was opened from the checklist.
pub fn mark_customize_read(vault_root: &Path) -> Result<()> {
    write_if_missing(&customize_marker(vault_root), "")
}

/// Removes the checklist — all steps done, or the user hid it. The step
/// markers stay; only `active` gates rendering, so this is idempotent and
/// a re-`activate` would come back pre-checked.
pub fn dismiss(vault_root: &Path) {
    match fs::remove_file(active_marker(vault_root)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => log::warn!("Thock: couldn't hide the Getting started list: {error}"),
    }
}

/// The marker files the panel's refresh snapshot folds in, so a change
/// re-renders without polling (V18 §6).
pub fn fingerprint(vault_root: &Path) -> Vec<(String, String)> {
    let mut fingerprint = Vec::new();
    for (name, path) in [
        ("getting-started:active", active_marker(vault_root)),
        (
            "getting-started:introduction",
            introduction_marker(vault_root),
        ),
        ("getting-started:customize", customize_marker(vault_root)),
        (
            "getting-started:tour",
            crate::routines::onboarding_marker_path(vault_root, WELCOME_TOUR_MARKER_ID),
        ),
    ] {
        if path.exists() {
            fingerprint.push((name.to_string(), String::new()));
        }
    }
    fingerprint
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_until_activated_then_steps_track_evidence() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(state(dir.path(), true), None);

        activate(dir.path()).unwrap();
        assert_eq!(
            state(dir.path(), false),
            Some(Steps {
                introduction: false,
                customize: false,
                agent: false,
                tour: false,
            })
        );

        mark_introduction_read(dir.path()).unwrap();
        mark_customize_read(dir.path()).unwrap();
        let marker = crate::routines::onboarding_marker_path(dir.path(), WELCOME_TOUR_MARKER_ID);
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "toured").unwrap();
        let steps = state(dir.path(), true).unwrap();
        assert!(steps.all_done());

        dismiss(dir.path());
        assert_eq!(state(dir.path(), true), None);
        // Dismissing twice is fine.
        dismiss(dir.path());
    }

    #[test]
    fn fingerprint_changes_with_each_marker() {
        let dir = tempfile::tempdir().unwrap();
        let empty = fingerprint(dir.path());
        activate(dir.path()).unwrap();
        let active = fingerprint(dir.path());
        assert_ne!(empty, active);
        mark_introduction_read(dir.path()).unwrap();
        assert_ne!(active, fingerprint(dir.path()));
    }
}
