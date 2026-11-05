//! What the client keeps for when the desk is away (spec §9.4–§9.5): a
//! cache of the last `context`, index, notes and searches, and an outbox of
//! writes that drains in order, once, when the desk is back. The formats
//! are plain files under the client's data directory.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// A cached text with the moment it was fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedText {
    pub as_of: String,
    pub text: String,
}

/// The suffix an offline answer carries (§9.4).
pub fn offline_suffix(as_of: &str, desk: &str) -> String {
    let when = DateTime::parse_from_rfc3339(as_of)
        .map(|at| at.with_timezone(&Utc).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| as_of.to_string());
    format!("\n\n(from cache, as of {when}; the desk at {desk} is unreachable)")
}

#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn write_json<T: Serialize>(&self, relative: &str, value: &T) -> std::io::Result<()> {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(value)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        fs::write(path, json)
    }

    fn read_json<T: for<'de> Deserialize<'de>>(&self, relative: &str) -> Option<T> {
        let raw = fs::read(self.root.join(relative)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    fn now() -> String {
        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    pub fn store_context(&self, text: &str) -> std::io::Result<()> {
        self.write_json(
            "context.json",
            &CachedText {
                as_of: Self::now(),
                text: text.to_string(),
            },
        )
    }

    pub fn context(&self) -> Option<CachedText> {
        self.read_json("context.json")
    }

    pub fn store_index(&self, text: &str) -> std::io::Result<()> {
        self.write_json(
            "index.json",
            &CachedText {
                as_of: Self::now(),
                text: text.to_string(),
            },
        )
    }

    pub fn index(&self) -> Option<CachedText> {
        self.read_json("index.json")
    }

    pub fn store_note(&self, path: &str, text: &str) -> std::io::Result<()> {
        self.write_json(
            &format!("notes/{}.json", crate::sha256_hex(path)),
            &CachedText {
                as_of: Self::now(),
                text: text.to_string(),
            },
        )
    }

    pub fn note(&self, path: &str) -> Option<CachedText> {
        self.read_json(&format!("notes/{}.json", crate::sha256_hex(path)))
    }

    /// A search's rendered answer, under `SearchQuery::cache_key`.
    pub fn store_search(&self, key: &str, text: &str) -> std::io::Result<()> {
        self.write_json(
            &format!("searches/{key}.json"),
            &CachedText {
                as_of: Self::now(),
                text: text.to_string(),
            },
        )
    }

    pub fn search(&self, key: &str) -> Option<CachedText> {
        self.read_json(&format!("searches/{key}.json"))
    }

    /// When the context was last refreshed, for `status`.
    pub fn age(&self) -> Option<String> {
        self.context().map(|cached| cached.as_of)
    }
}

/// One queued call (§9.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub id: String,
    pub at: String,
    pub tool: String,
    pub args: Value,
    /// The harness that queued it, so the line lands under its own name
    /// whichever `serve` drains it.
    #[serde(default)]
    pub harness: String,
}

impl OutboxEntry {
    pub fn new(tool: &str, args: Value, harness: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            tool: tool.to_string(),
            args,
            harness: harness.to_string(),
        }
    }

    /// The day the agent called, as the queued line keeps it.
    pub fn date(&self) -> Option<chrono::NaiveDate> {
        DateTime::parse_from_rfc3339(&self.at)
            .ok()
            .map(|at| at.with_timezone(&chrono::Local).date_naive())
    }
}

/// How sending a queued call went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainFailure {
    /// The desk did not answer: keep the entry and stop, so order holds.
    Transport(String),
    /// The desk answered and said no (a 4xx, a `-32602`/`-32601`): the entry
    /// can never land; drop it and carry on.
    Refused(String),
}

/// Where a drain stopped: at a desk that did not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainStop {
    pub entry: OutboxEntry,
    pub reason: String,
}

/// What a drain did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainReport {
    pub sent: usize,
    /// Entries the desk refused for good, with its reason.
    pub refused: Vec<(OutboxEntry, String)>,
    pub stopped: Option<DrainStop>,
}

#[derive(Debug, Clone)]
pub struct Outbox {
    path: PathBuf,
}

/// An advisory lock over the outbox, held while one `serve` pushes or
/// drains so two harnesses on one machine never interleave half-written
/// lines or drain the same entry twice.
struct OutboxLock {
    _file: fs::File,
}

impl OutboxLock {
    fn take(path: &Path) -> std::io::Result<Self> {
        let lock_path = path.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd as _;
            // SAFETY: the descriptor is ours for as long as `file` lives;
            // the lock goes when it closes.
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if result != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(Self { _file: file })
    }
}

impl Outbox {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn push(&self, entry: &OutboxEntry) -> std::io::Result<()> {
        let _lock = OutboxLock::take(&self.path)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(entry)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        writeln!(file, "{line}")
    }

    pub fn entries(&self) -> Vec<OutboxEntry> {
        let Ok(raw) = fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        raw.lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn rewrite(&self, entries: &[OutboxEntry]) -> std::io::Result<()> {
        if entries.is_empty() {
            match fs::remove_file(&self.path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        let mut text = String::new();
        for entry in entries {
            let line = serde_json::to_string(entry)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            text.push_str(&line);
            text.push('\n');
        }
        fs::write(&self.path, text)
    }

    /// Sends the queued calls in order through `send`, dropping each one
    /// the moment it lands or the desk refuses it for good, and stopping at
    /// the first desk that does not answer so order holds. The desk treats
    /// the entry's id as idempotent, so a reply lost in flight is retried
    /// without a second write.
    pub fn drain(
        &self,
        mut send: impl FnMut(&OutboxEntry) -> Result<(), DrainFailure>,
    ) -> DrainReport {
        let mut report = DrainReport::default();
        let _lock = match OutboxLock::take(&self.path) {
            Ok(lock) => lock,
            Err(error) => {
                report.stopped = self.entries().into_iter().next().map(|entry| DrainStop {
                    entry,
                    reason: format!("couldn't lock the outbox: {error}"),
                });
                return report;
            }
        };
        let mut remaining = self.entries();
        while !remaining.is_empty() {
            let entry = remaining[0].clone();
            match send(&entry) {
                Ok(()) => report.sent += 1,
                Err(DrainFailure::Refused(reason)) => {
                    report.refused.push((entry.clone(), reason));
                }
                Err(DrainFailure::Transport(reason)) => {
                    report.stopped = Some(DrainStop { entry, reason });
                    return report;
                }
            }
            remaining.remove(0);
            if let Err(error) = self.rewrite(&remaining) {
                report.stopped = Some(DrainStop {
                    entry,
                    reason: format!("couldn't update the outbox: {error}"),
                });
                return report;
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_cache_round_trips_and_marks_its_age() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().join("cache"));
        assert!(cache.context().is_none());
        cache.store_context("# Right now").unwrap();
        let cached = cache.context().unwrap();
        assert_eq!(cached.text, "# Right now");
        assert!(DateTime::parse_from_rfc3339(&cached.as_of).is_ok());
        cache.store_note("daily/x.md", "body").unwrap();
        assert_eq!(cache.note("daily/x.md").unwrap().text, "body");
        assert!(cache.note("daily/y.md").is_none());
        cache.store_index("# Learned").unwrap();
        assert_eq!(cache.index().unwrap().text, "# Learned");
        cache.store_search("key", "- `a.md` — A").unwrap();
        assert_eq!(cache.search("key").unwrap().text, "- `a.md` — A");
        assert_eq!(cache.age(), Some(cached.as_of));
        assert_eq!(
            offline_suffix("2026-11-04T08:12:03Z", "palloma.local:7557"),
            "\n\n(from cache, as of 2026-11-04 08:12; the desk at palloma.local:7557 is unreachable)"
        );
    }

    #[test]
    fn the_outbox_drains_in_order_and_stops_at_the_first_failure() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(dir.path().join("outbox.jsonl"));
        assert!(outbox.is_empty());
        let first = OutboxEntry::new("remember", json!({"fact": "one"}), "claude-code");
        let second = OutboxEntry::new("note_today", json!({"line": "two"}), "codex");
        let third = OutboxEntry::new("remember", json!({"fact": "three"}), "claude-code");
        outbox.push(&first).unwrap();
        outbox.push(&second).unwrap();
        outbox.push(&third).unwrap();
        assert_eq!(outbox.len(), 3);
        assert!(first.date().is_some());
        assert_eq!(outbox.entries()[1].harness, "codex");

        let mut sent = Vec::new();
        let report = outbox.drain(|entry| {
            if entry.id == second.id {
                return Err(DrainFailure::Transport("the desk is unreachable".into()));
            }
            sent.push(entry.id.clone());
            Ok(())
        });
        assert_eq!(report.sent, 1);
        assert_eq!(
            report.stopped.as_ref().map(|stop| &stop.entry),
            Some(&second)
        );
        assert_eq!(sent, vec![first.id]);
        assert_eq!(outbox.entries(), vec![second, third]);

        let report = outbox.drain(|_| Ok(()));
        assert_eq!(report.sent, 2);
        assert!(report.stopped.is_none());
        assert!(outbox.is_empty());
        assert!(!outbox.path().exists());
        assert_eq!(outbox.drain(|_| Ok(())).sent, 0);
    }

    #[test]
    fn a_refused_entry_is_dropped_and_the_rest_still_goes() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(dir.path().join("outbox.jsonl"));
        let bad = OutboxEntry::new("remember", json!({"fact": ""}), "codex");
        let good = OutboxEntry::new("remember", json!({"fact": "fine"}), "codex");
        outbox.push(&bad).unwrap();
        outbox.push(&good).unwrap();
        let report = outbox.drain(|entry| {
            if entry.id == bad.id {
                Err(DrainFailure::Refused("the line is empty".into()))
            } else {
                Ok(())
            }
        });
        assert_eq!(report.sent, 1);
        assert_eq!(report.refused, vec![(bad, "the line is empty".to_string())]);
        assert!(report.stopped.is_none());
        assert!(outbox.is_empty());
    }

    #[test]
    fn the_lock_file_sits_beside_the_outbox() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(dir.path().join("outbox.jsonl"));
        outbox
            .push(&OutboxEntry::new("remember", json!({"fact": "x"}), "codex"))
            .unwrap();
        assert!(dir.path().join("outbox.lock").exists());
        let held = OutboxLock::take(outbox.path()).unwrap();
        drop(held);
        assert_eq!(outbox.drain(|_| Ok(())).sent, 1);
    }
}
