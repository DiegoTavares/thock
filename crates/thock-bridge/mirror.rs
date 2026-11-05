//! One-way mirroring of a harness's own memory folder into the vault (spec
//! §9.6): which files go, and the record of what already went.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::rules::validate_vault_path;

pub const MIRROR_MAX_BYTES: u64 = 1024 * 1024;
const MIRRORED_EXTENSIONS: [&str; 3] = ["md", "txt", "json"];

/// A file due for upload: its path under the harness folder and on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorFile {
    pub relative: String,
    pub absolute: PathBuf,
    pub modified_secs: u64,
}

/// `mirror-state.json`: the mtime each file had when it last went up.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorState {
    #[serde(default)]
    pub uploaded: BTreeMap<String, u64>,
}

impl MirrorState {
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        fs::write(path, json)
    }

    pub fn record(&mut self, relative: &str, modified_secs: u64) {
        self.uploaded.insert(relative.to_string(), modified_secs);
    }
}

/// Whether a path under the mirror folder is one the desk accepts: a
/// `.md`, `.txt` or `.json` file at a valid vault-relative path.
pub fn is_mirrored_path(relative: &str) -> bool {
    validate_vault_path(relative).is_ok()
        && relative
            .rsplit('.')
            .next()
            .is_some_and(|extension| {
                MIRRORED_EXTENSIONS
                    .iter()
                    .any(|allowed| extension.eq_ignore_ascii_case(allowed))
            })
}

/// The files under `root` newer than their last upload, in path order.
/// Oversized files and dot-paths are skipped in silence: they are the
/// harness's business.
pub fn plan_mirror(root: &Path, state: &MirrorState) -> Vec<MirrorFile> {
    let mut found = Vec::new();
    walk(root, root, state, &mut found);
    found.sort_by(|a, b| a.relative.cmp(&b.relative));
    found
}

fn walk(root: &Path, dir: &Path, state: &MirrorState, found: &mut Vec<MirrorFile>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.starts_with('.'))
            {
                walk(root, &path, state, found);
            }
            continue;
        }
        if !metadata.is_file() || metadata.len() > MIRROR_MAX_BYTES {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let relative = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if !is_mirrored_path(&relative) {
            continue;
        }
        let modified_secs = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let already = state.uploaded.get(&relative).copied();
        if already.is_some_and(|uploaded| uploaded >= modified_secs) {
            continue;
        }
        found.push(MirrorFile {
            relative,
            absolute: path,
            modified_secs,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_small_text_files_at_clean_paths_are_planned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("topics")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join("MEMORY.md"), "# index").unwrap();
        fs::write(root.join("topics/a.md"), "a").unwrap();
        fs::write(root.join("topics/data.json"), "{}").unwrap();
        fs::write(root.join("topics/notes.txt"), "t").unwrap();
        fs::write(root.join("image.png"), "png").unwrap();
        fs::write(root.join(".hidden/x.md"), "x").unwrap();
        fs::write(root.join(".secret.md"), "x").unwrap();
        fs::write(root.join("big.md"), vec![b'x'; (MIRROR_MAX_BYTES + 1) as usize]).unwrap();
        let planned = plan_mirror(root, &MirrorState::default());
        let paths: Vec<&str> = planned.iter().map(|file| file.relative.as_str()).collect();
        assert_eq!(
            paths,
            ["MEMORY.md", "topics/a.md", "topics/data.json", "topics/notes.txt"]
        );
    }

    #[test]
    fn the_state_skips_what_already_went_up() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("MEMORY.md"), "# index").unwrap();
        let planned = plan_mirror(root, &MirrorState::default());
        assert_eq!(planned.len(), 1);
        let mut state = MirrorState::default();
        state.record("MEMORY.md", planned[0].modified_secs);
        assert!(plan_mirror(root, &state).is_empty());
        state.record("MEMORY.md", planned[0].modified_secs - 1);
        assert_eq!(plan_mirror(root, &state).len(), 1);
        let state_path = root.join("state/mirror-state.json");
        state.save(&state_path).unwrap();
        assert_eq!(MirrorState::load(&state_path), state);
        assert_eq!(MirrorState::load(&root.join("missing.json")), MirrorState::default());
    }

    #[test]
    fn mirrored_paths_follow_the_vault_rules() {
        assert!(is_mirrored_path("MEMORY.md"));
        assert!(is_mirrored_path("a/b.JSON"));
        assert!(!is_mirrored_path("a/.b.md"));
        assert!(!is_mirrored_path("../x.md"));
        assert!(!is_mirrored_path("x.png"));
    }
}
