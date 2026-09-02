use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::day_plan::DayPlannerConfig;
use crate::notes::NoteKind;

pub const VAULT_MARKER_DIR: &str = ".thock";
pub const VAULT_CONFIG_FILE: &str = "config.toml";
pub const WELCOME_FILE: &str = "welcome.md";

pub const DEFAULT_CONFIG_TOML: &str = r#"schema = 1

[daily]
dir      = "daily"           # daily notes dir, relative to vault root
filename = "YYYY-MM-DD"      # moment-style date format; ".md" is appended
template = "templates/daily.md"

[weekly]
dir      = "weekly"          # weekly notes dir, relative to vault root
filename = "GGGG-[W]WW"      # ISO week year + week number, e.g. 2026-W30
template = "templates/weekly.md"

[backlog]
file = "backlog.md"          # the Soon / Someday / Completed holding pen

# [markdown]
# conceal = true             # hide markup while the cursor is off the line
# email_view = true          # render synced email notes as conversations

[[routines.installed]]
id      = "timeline"
enabled = true
version = 7

[[routines.installed]]
id      = "inbox"
enabled = true
version = 1
"#;

pub const DEFAULT_DAILY_TEMPLATE: &str = r#"# {{date:dddd, MMMM D, YYYY}}

## Journal

## Day planner

## Personal
"#;

pub const DEFAULT_WEEKLY_TEMPLATE: &str = r#"# Week {{date:W}}, {{date:GGGG}}

## Goals

## Notes

## Week review
"#;

pub const DEFAULT_WELCOME: &str = r#"# Welcome to Thock

This folder is your **vault** — a plain folder of Markdown files that belongs to you.

## The Timeline

Open the **Timeline** panel in the left sidebar and click an entry:

- **Today** / **Yesterday** open daily notes, created from `templates/daily.md`.
- **This Week** / **Last Week** open weekly notes, created from `templates/weekly.md`.

The same entries (plus **Tomorrow**) are available from the command palette as
`thock: open today`, `thock: open tomorrow`, and friends.

Notes live in `daily/` and `weekly/`, one file per day or week. Existing notes
are only ever opened — never overwritten.

## Make it yours

Everything is a plain file you can edit:

- `templates/daily.md` and `templates/weekly.md` — the templates for new notes.
  Tokens like `{{date:dddd, MMMM D, YYYY}}`, `{{time}}`, and `{{title}}` are
  filled in when a note is created.
- `.thock/config.toml` — where notes go and how they are named.

This file is just a note, too. Edit it, or delete it once you've found your feet.
"#;

/// The parsed shape of `config.toml`; every field is optional so partially
/// specified sections fall back to defaults. Also `Serialize` so the
/// routines registry can rewrite the file: only fields the user actually set
/// are re-emitted (comments are not preserved).
///
/// `deny_unknown_fields` stays deliberately (V7 §9 trap 4): any new registry
/// key makes older builds treat the whole vault as invalid, so new state
/// belongs in `.thock/routines/<id>/`, not here.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct VaultConfigContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<u32>,
    #[serde(skip_serializing_if = "NotesConfigContent::is_unset")]
    daily: NotesConfigContent,
    #[serde(skip_serializing_if = "NotesConfigContent::is_unset")]
    weekly: NotesConfigContent,
    #[serde(skip_serializing_if = "HistoryConfigContent::is_unset")]
    history: HistoryConfigContent,
    /// The pre-V7 registry key. Read for compatibility, merged into
    /// `routines`, and never written back — a registry rewrite migrates the
    /// vault to `[[routines.installed]]` (V7 decision 4).
    #[serde(skip_serializing_if = "RoutinesConfigContent::is_unset")]
    areas: RoutinesConfigContent,
    #[serde(skip_serializing_if = "RoutinesConfigContent::is_unset")]
    routines: RoutinesConfigContent,
    #[serde(skip_serializing_if = "DayPlannerConfigContent::is_unset")]
    day_planner: DayPlannerConfigContent,
    #[serde(skip_serializing_if = "AgentConfigContent::is_unset")]
    agent: AgentConfigContent,
    #[serde(skip_serializing_if = "BacklogConfigContent::is_unset")]
    backlog: BacklogConfigContent,
    #[serde(skip_serializing_if = "MarkdownConfigContent::is_unset")]
    markdown: MarkdownConfigContent,
}

impl VaultConfigContent {
    /// The registry with both keys merged: `[[routines.installed]]` wins on
    /// duplicate ids, `[[areas.installed]]` entries follow.
    fn merged_registry(&mut self) -> Vec<InstalledRoutine> {
        let mut installed: Vec<InstalledRoutine> = self
            .routines
            .installed
            .drain(..)
            .map(InstalledRoutineContent::resolve)
            .collect();
        for legacy in self.areas.installed.drain(..) {
            let legacy = legacy.resolve();
            if !installed.iter().any(|entry| entry.id == legacy.id) {
                installed.push(legacy);
            }
        }
        installed
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct BacklogConfigContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
}

impl BacklogConfigContent {
    fn resolve(self) -> BacklogConfig {
        BacklogConfig {
            file: self
                .file
                .filter(|file| !file.trim().is_empty())
                .unwrap_or_else(|| BacklogConfig::default().file),
        }
    }

    fn is_unset(&self) -> bool {
        self.file.is_none()
    }
}

/// The `[backlog]` table: where the vault's backlog file lives (spec
/// `v6-backlog.md` §5.2). One file per vault.
#[derive(Debug, Clone, PartialEq)]
pub struct BacklogConfig {
    /// Vault-relative path of the backlog file.
    pub file: String,
}

impl Default for BacklogConfig {
    fn default() -> Self {
        Self {
            file: "backlog.md".to_string(),
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct AgentConfigContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fast_command: Option<String>,
}

impl AgentConfigContent {
    fn resolve(self) -> AgentConfig {
        AgentConfig {
            command: self.command.filter(|command| !command.trim().is_empty()),
            fast_command: self
                .fast_command
                .filter(|command| !command.trim().is_empty()),
        }
    }

    fn is_unset(&self) -> bool {
        self.command.is_none() && self.fast_command.is_none()
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct MarkdownConfigContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    conceal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email_view: Option<bool>,
}

impl MarkdownConfigContent {
    fn resolve(self) -> MarkdownConfig {
        MarkdownConfig {
            conceal: self.conceal.unwrap_or(true),
            email_view: self.email_view.unwrap_or(true),
        }
    }

    fn is_unset(&self) -> bool {
        self.conceal.is_none() && self.email_view.is_none()
    }
}

/// The `[markdown]` table: whether Markdown markup in vault notes is hidden
/// while the cursor is off the line (spec V10 §9), and whether synced email
/// notes render with the email view (spec V16 §4). Both default to on.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownConfig {
    pub conceal: bool,
    pub email_view: bool,
}

impl Default for MarkdownConfig {
    fn default() -> Self {
        Self {
            conceal: true,
            email_view: true,
        }
    }
}

/// The `[agent]` table: this vault's launch-command overrides for the user's
/// CLI agent. When absent, the user-level defaults apply (see
/// `crate::agent`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentConfig {
    pub command: Option<String>,
    /// The command for skills that declare `model = "fast"` — where
    /// CLI-specific model flags belong, so Routines never carry them.
    pub fast_command: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct DayPlannerConfigContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    heading: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    day_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    day_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_duration_minutes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    show_now_indicator: Option<bool>,
    /// Palette-index pins for subsection colours (spec v8 §7.3). A `BTreeMap`
    /// so re-serialization is deterministic.
    #[serde(skip_serializing_if = "Option::is_none")]
    sections: Option<std::collections::BTreeMap<String, usize>>,
}

impl DayPlannerConfigContent {
    fn resolve(self) -> DayPlannerConfig {
        let defaults = DayPlannerConfig::default();
        let parse_bound = |raw: Option<String>, name: &str, default: u32| match raw {
            None => default,
            Some(raw) => match crate::day_plan::parse_grid_bound(&raw) {
                Some(minutes) => minutes,
                None => {
                    log::warn!(
                        "Thock: invalid [day_planner].{name} {raw:?} in config.toml; \
                         using the default"
                    );
                    default
                }
            },
        };
        let mut day_start = parse_bound(self.day_start, "day_start", defaults.day_start);
        let mut day_end = parse_bound(self.day_end, "day_end", defaults.day_end);
        if day_end <= day_start {
            log::warn!(
                "Thock: [day_planner] day_end must be after day_start; using the defaults"
            );
            day_start = defaults.day_start;
            day_end = defaults.day_end;
        }
        DayPlannerConfig {
            heading: self.heading.unwrap_or(defaults.heading),
            day_start,
            day_end,
            default_duration: match self.default_duration_minutes {
                Some(0) => {
                    log::warn!(
                        "Thock: [day_planner].default_duration_minutes must be at \
                         least 1; using the default"
                    );
                    defaults.default_duration
                }
                Some(minutes) => minutes,
                None => defaults.default_duration,
            },
            show_now_indicator: self
                .show_now_indicator
                .unwrap_or(defaults.show_now_indicator),
            sections: self
                .sections
                .unwrap_or_default()
                .into_iter()
                .map(|(name, index)| (name.trim().to_lowercase(), index))
                .collect(),
        }
    }

    fn is_unset(&self) -> bool {
        self.heading.is_none()
            && self.day_start.is_none()
            && self.day_end.is_none()
            && self.default_duration_minutes.is_none()
            && self.show_now_indicator.is_none()
            && self.sections.is_none()
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct NotesConfigContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<String>,
}

impl NotesConfigContent {
    fn resolve(self, defaults: NotesConfig) -> NotesConfig {
        NotesConfig {
            dir: self.dir.unwrap_or(defaults.dir),
            filename: self.filename.unwrap_or(defaults.filename),
            template: self.template.unwrap_or(defaults.template),
        }
    }

    fn is_unset(&self) -> bool {
        self.dir.is_none() && self.filename.is_none() && self.template.is_none()
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct HistoryConfigContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idle_debounce_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    heartbeat_minutes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_file_bytes: Option<u64>,
}

impl HistoryConfigContent {
    fn resolve(self) -> HistoryConfig {
        HistoryConfig {
            enabled: self.enabled.unwrap_or(true),
            idle_debounce: Duration::from_secs(self.idle_debounce_seconds.unwrap_or(20)),
            heartbeat: Duration::from_secs(self.heartbeat_minutes.unwrap_or(5) * 60),
            max_file_bytes: self.max_file_bytes.unwrap_or(2_000_000),
        }
    }

    fn is_unset(&self) -> bool {
        self.enabled.is_none()
            && self.idle_debounce_seconds.is_none()
            && self.heartbeat_minutes.is_none()
            && self.max_file_bytes.is_none()
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct RoutinesConfigContent {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    installed: Vec<InstalledRoutineContent>,
}

impl RoutinesConfigContent {
    fn is_unset(&self) -> bool {
        self.installed.is_empty()
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstalledRoutineContent {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    onboarding_installed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    onboarding_state: Option<String>,
}

impl InstalledRoutineContent {
    fn resolve(self) -> InstalledRoutine {
        InstalledRoutine {
            id: self.id,
            enabled: self.enabled.unwrap_or(true),
            version: self.version.unwrap_or(1),
            onboarding_installed_at: self.onboarding_installed_at.as_deref().and_then(|raw| {
                match chrono::DateTime::parse_from_rfc3339(raw) {
                    Ok(timestamp) => Some(timestamp.with_timezone(&chrono::Utc)),
                    Err(error) => {
                        log::warn!(
                            "Thock: invalid onboarding_installed_at {raw:?} in \
                             config.toml: {error}"
                        );
                        None
                    }
                }
            }),
            onboarding_state: match self.onboarding_state.as_deref() {
                None => None,
                Some("pending") => Some(OnboardingState::Pending),
                Some("onboarded") => Some(OnboardingState::Onboarded),
                Some("expired") => Some(OnboardingState::Expired),
                Some(other) => {
                    log::warn!(
                        "Thock: unknown onboarding_state {other:?} in config.toml; \
                         treating the Routine as expired"
                    );
                    Some(OnboardingState::Expired)
                }
            },
        }
    }
}

/// Where an installed Routine stands in the agentic-onboarding flow (V5
/// §7.4). `None` on an entry means the Routine was installed before V5 (or
/// scaffolded rather than added), which the UI treats like `Expired`: the
/// quiet "Set up with AI" action only.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OnboardingState {
    Pending,
    Onboarded,
    Expired,
}

impl OnboardingState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Onboarded => "onboarded",
            Self::Expired => "expired",
        }
    }
}

/// The `[[routines.installed]]` registry (V7 spec §2; formerly
/// `[[areas.installed]]`, which is still read). Array order is display order
/// in the panel.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoutinesConfig {
    pub installed: Vec<InstalledRoutine>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstalledRoutine {
    pub id: String,
    pub enabled: bool,
    pub version: u32,
    pub onboarding_installed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub onboarding_state: Option<OnboardingState>,
}

impl InstalledRoutine {
    pub fn new(id: String, enabled: bool, version: u32) -> Self {
        Self {
            id,
            enabled,
            version,
            onboarding_installed_at: None,
            onboarding_state: None,
        }
    }

    fn into_content(self) -> InstalledRoutineContent {
        InstalledRoutineContent {
            id: self.id,
            enabled: Some(self.enabled),
            version: Some(self.version),
            onboarding_installed_at: self
                .onboarding_installed_at
                .map(|timestamp| timestamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            onboarding_state: self
                .onboarding_state
                .map(|state| state.as_str().to_string()),
        }
    }
}

/// Settings for the invisible checkpoint history (the `[history]` table).
/// Every field has a default, so the table may be entirely absent.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryConfig {
    pub enabled: bool,
    pub idle_debounce: Duration,
    pub heartbeat: Duration,
    pub max_file_bytes: u64,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        HistoryConfigContent::default().resolve()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VaultConfig {
    pub schema: u32,
    pub daily: NotesConfig,
    pub weekly: NotesConfig,
    pub history: HistoryConfig,
    pub routines: RoutinesConfig,
    pub day_planner: DayPlannerConfig,
    pub agent: AgentConfig,
    pub backlog: BacklogConfig,
    pub markdown: MarkdownConfig,
}

impl Default for VaultConfig {
    fn default() -> Self {
        VaultConfigContent::default().resolve()
    }
}

impl VaultConfigContent {
    fn resolve(mut self) -> VaultConfig {
        let installed = self.merged_registry();
        VaultConfig {
            schema: self.schema.unwrap_or(1),
            daily: self.daily.resolve(NotesConfig::daily_default()),
            weekly: self.weekly.resolve(NotesConfig::weekly_default()),
            history: self.history.resolve(),
            routines: RoutinesConfig { installed },
            day_planner: self.day_planner.resolve(),
            agent: self.agent.resolve(),
            backlog: self.backlog.resolve(),
            markdown: self.markdown.resolve(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotesConfig {
    pub dir: String,
    pub filename: String,
    pub template: String,
}

impl NotesConfig {
    fn daily_default() -> Self {
        Self {
            dir: "daily".to_string(),
            filename: "YYYY-MM-DD".to_string(),
            template: "templates/daily.md".to_string(),
        }
    }

    fn weekly_default() -> Self {
        Self {
            dir: "weekly".to_string(),
            filename: "GGGG-[W]WW".to_string(),
            template: "templates/weekly.md".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Vault {
    pub root: PathBuf,
    pub config: VaultConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VaultStatus {
    /// The folder has no `.thock/` marker.
    NotAVault,
    /// The marker exists but `config.toml` could not be read or parsed.
    Invalid {
        error: String,
    },
    Valid(Vault),
}

impl Vault {
    /// Determines whether `root` is a Thock vault and loads its config.
    pub fn detect(root: &Path) -> VaultStatus {
        let config_path = root.join(VAULT_MARKER_DIR).join(VAULT_CONFIG_FILE);
        if !config_path.is_file() {
            return VaultStatus::NotAVault;
        }
        match fs::read_to_string(&config_path) {
            Ok(contents) => match toml::from_str::<VaultConfigContent>(&contents) {
                Ok(content) => VaultStatus::Valid(Vault {
                    root: root.to_path_buf(),
                    config: content.resolve(),
                }),
                Err(error) => VaultStatus::Invalid {
                    error: format!("failed to parse {}: {}", config_path.display(), error),
                },
            },
            Err(error) => VaultStatus::Invalid {
                error: format!("failed to read {}: {}", config_path.display(), error),
            },
        }
    }

    pub fn notes_config(&self, kind: NoteKind) -> &NotesConfig {
        match kind {
            NoteKind::Daily => &self.config.daily,
            NoteKind::Weekly => &self.config.weekly,
        }
    }

    pub fn note_path(&self, kind: NoteKind, date: chrono::NaiveDate) -> PathBuf {
        let config = self.notes_config(kind);
        let stem = crate::notes::format_date(date, &config.filename);
        self.root.join(&config.dir).join(format!("{stem}.md"))
    }

    pub fn template_path(&self, kind: NoteKind) -> PathBuf {
        self.root.join(&self.notes_config(kind).template)
    }

    /// The absolute path of the vault's backlog file (spec `v6-backlog.md`).
    pub fn backlog_path(&self) -> PathBuf {
        self.root.join(&self.config.backlog.file)
    }

    /// The date of the daily note at `path`, or `None` when `path` isn't a
    /// daily note of this vault. The inverse of `note_path(NoteKind::Daily, _)`:
    /// the whole path below the daily directory is matched against the
    /// filename format, so formats containing `/` (nested folders per year or
    /// month) are recognized too.
    pub fn daily_note_date(&self, path: &Path) -> Option<chrono::NaiveDate> {
        let daily_dir = self.root.join(&self.config.daily.dir);
        let relative = path.strip_prefix(&daily_dir).ok()?.to_str()?;
        let stem = relative.strip_suffix(".md")?;
        crate::notes::parse_date(stem, &self.config.daily.filename)
    }
}

/// Creates `path`'s parent directories and writes `contents`, unless the file
/// already exists — scaffolding and Routine materialization never clobber
/// user data.
pub(crate) fn write_if_missing(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    if !path.exists() {
        fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Serializes the read-modify-write config rewrites below. They run on
/// arbitrary background threads (user actions racing the onboarding watcher),
/// and two interleaved rewrites would silently drop one side's change.
static CONFIG_REWRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_config_rewrites() -> std::sync::MutexGuard<'static, ()> {
    // The guarded data is `()`, so a panic while holding the lock can't have
    // left anything inconsistent — recover instead of poisoning forever.
    CONFIG_REWRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Rewrites `.thock/config.toml` with `mutate` applied to the Routines
/// registry. Both registry keys are read (`[[routines.installed]]` wins on
/// duplicates), and only the new key is written back, so any rewrite
/// migrates a pre-V7 vault off `[[areas.installed]]` (V7 decision 4).
/// Re-serializes the known config schema, so only fields present in the file
/// are kept and comments are dropped. Blocking I/O — call from a background
/// thread.
pub fn update_routines_registry(
    root: &Path,
    mutate: impl FnOnce(&mut Vec<InstalledRoutine>),
) -> Result<()> {
    let _rewrite_lock = lock_config_rewrites();
    let config_path = root.join(VAULT_MARKER_DIR).join(VAULT_CONFIG_FILE);
    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let mut content: VaultConfigContent =
        toml::from_str(&raw).with_context(|| format!("parsing {}", config_path.display()))?;

    let mut installed = content.merged_registry();
    mutate(&mut installed);
    content.routines.installed = installed
        .into_iter()
        .map(InstalledRoutine::into_content)
        .collect();

    let serialized = toml::to_string_pretty(&content).context("serializing vault config")?;
    fs::write(&config_path, serialized)
        .with_context(|| format!("writing {}", config_path.display()))?;
    Ok(())
}

/// Rewrites `.thock/config.toml` with the `[agent] command` override set
/// (or cleared with `None`). Re-serializes the known schema like
/// `update_routines_registry`. Blocking I/O — call from a background thread.
pub fn update_agent_command(root: &Path, command: Option<String>) -> Result<()> {
    let _rewrite_lock = lock_config_rewrites();
    let config_path = root.join(VAULT_MARKER_DIR).join(VAULT_CONFIG_FILE);
    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let mut content: VaultConfigContent =
        toml::from_str(&raw).with_context(|| format!("parsing {}", config_path.display()))?;
    content.agent.command = command;
    let serialized = toml::to_string_pretty(&content).context("serializing vault config")?;
    fs::write(&config_path, serialized)
        .with_context(|| format!("writing {}", config_path.display()))?;
    Ok(())
}

/// Writes the default vault structure into `root`, creating it if needed.
///
/// Only files that don't exist yet are written, so scaffolding into a non-empty
/// folder (the "Create vault here" action) never clobbers user data.
pub fn scaffold_vault(root: &Path) -> Result<()> {
    let config_path = root.join(VAULT_MARKER_DIR).join(VAULT_CONFIG_FILE);
    // The Timeline Routine ships pre-installed, but only when this scaffold
    // is creating the vault: an existing config.toml wouldn't register the
    // Routine, and materializing unregistered files would clutter an
    // existing vault.
    let install_default_routines = !config_path.exists();
    write_if_missing(&config_path, DEFAULT_CONFIG_TOML)?;
    fs::create_dir_all(root.join("daily")).context("creating daily dir")?;
    fs::create_dir_all(root.join("weekly")).context("creating weekly dir")?;
    write_if_missing(
        &root.join("templates").join("daily.md"),
        DEFAULT_DAILY_TEMPLATE,
    )?;
    write_if_missing(
        &root.join("templates").join("weekly.md"),
        DEFAULT_WEEKLY_TEMPLATE,
    )?;
    write_if_missing(&root.join(WELCOME_FILE), DEFAULT_WELCOME)?;
    write_if_missing(
        &root.join(BacklogConfig::default().file),
        crate::backlog::DEFAULT_BACKLOG,
    )?;
    crate::routines::materialize_core_files(root)?;
    if install_default_routines {
        // Timeline and Inbox ship pre-installed (V13 §12 #3): the front door
        // exists from first run, so capture always has a ritual to point at.
        for routine_id in [
            crate::routines::TIMELINE_ROUTINE_ID,
            crate::routines::INBOX_ROUTINE_ID,
        ] {
            let routine = crate::routines::catalog_routine(routine_id)?.with_context(|| {
                format!("the bundled {routine_id} Routine is missing from the catalog")
            })?;
            crate::routines::materialize_routine(root, &routine)?;
        }
    }
    Ok(())
}

/// The default location for the vault scaffolded on first run.
pub fn default_vault_path() -> PathBuf {
    util::paths::home_dir().join("Thock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn detect_non_vault() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Vault::detect(dir.path()), VaultStatus::NotAVault);
    }

    #[test]
    fn detect_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(marker.join(VAULT_CONFIG_FILE), "not [valid toml").unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Invalid { .. } => {}
            other => panic!("expected invalid, got {other:?}"),
        }
    }

    #[test]
    fn scaffold_then_detect_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let vault = match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => vault,
            other => panic!("expected valid vault, got {other:?}"),
        };
        assert_eq!(vault.config.daily, VaultConfig::default().daily);
        assert_eq!(vault.config.weekly, VaultConfig::default().weekly);
        assert_eq!(vault.config.history, VaultConfig::default().history);
        assert_eq!(
            vault.config.routines.installed,
            vec![
                InstalledRoutine::new("timeline".to_string(), true, 7),
                InstalledRoutine::new("inbox".to_string(), true, 1),
            ]
        );
        // The Inbox Routine's landing zone and files ship with the scaffold.
        assert!(dir.path().join("inbox").is_dir());
        assert!(dir.path().join("routines/inbox/routine.toml").is_file());
        assert!(dir.path().join("routines/inbox/triage-policy.md").is_file());
        assert!(
            dir.path()
                .join("routines/inbox/skills/triage-inbox.md")
                .is_file()
        );
        assert!(dir.path().join("daily").is_dir());
        assert!(dir.path().join("weekly").is_dir());
        assert!(dir.path().join("templates/daily.md").is_file());
        assert!(dir.path().join("templates/weekly.md").is_file());
        assert!(dir.path().join(WELCOME_FILE).is_file());
        assert!(dir.path().join("backlog.md").is_file());
        assert_eq!(vault.backlog_path(), dir.path().join("backlog.md"));

        let date = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
        assert_eq!(
            vault.note_path(NoteKind::Daily, date),
            dir.path().join("daily/2026-07-20.md")
        );
        assert_eq!(
            vault.note_path(NoteKind::Weekly, date),
            dir.path().join("weekly/2026-W30.md")
        );
    }

    #[test]
    fn scaffold_preserves_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(WELCOME_FILE), "my own welcome").unwrap();
        scaffold_vault(dir.path()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join(WELCOME_FILE)).unwrap(),
            "my own welcome"
        );
    }

    #[test]
    fn history_config_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n[history]\nenabled = false\nidle_debounce_seconds = 5\n",
        )
        .unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                assert!(!vault.config.history.enabled);
                assert_eq!(vault.config.history.idle_debounce, Duration::from_secs(5));
                // Unspecified keys keep their defaults.
                assert_eq!(vault.config.history.heartbeat, Duration::from_secs(300));
                assert_eq!(vault.config.history.max_file_bytes, 2_000_000);
            }
            other => panic!("expected valid vault, got {other:?}"),
        }
    }

    #[test]
    fn update_routines_registry_preserves_other_config() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n\n[daily]\ndir = \"notes/daily\"\n\n[history]\nenabled = false\n",
        )
        .unwrap();

        update_routines_registry(dir.path(), |installed| {
            installed.push(InstalledRoutine::new("timeline".to_string(), true, 1));
        })
        .unwrap();
        update_routines_registry(dir.path(), |installed| {
            if let Some(entry) = installed.first_mut() {
                entry.enabled = false;
            }
        })
        .unwrap();

        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                assert_eq!(vault.config.daily.dir, "notes/daily");
                // Unset fields must stay unset, not be materialized as defaults.
                assert_eq!(vault.config.daily.filename, "YYYY-MM-DD");
                assert!(!vault.config.history.enabled);
                assert_eq!(vault.config.weekly, NotesConfig::weekly_default());
                assert_eq!(
                    vault.config.routines.installed,
                    vec![InstalledRoutine::new("timeline".to_string(), false, 1)]
                );
            }
            other => panic!("expected valid vault, got {other:?}"),
        }
        let raw = fs::read_to_string(marker.join(VAULT_CONFIG_FILE)).unwrap();
        assert!(!raw.contains("[weekly]"), "unset section reappeared: {raw}");
    }

    #[test]
    fn legacy_areas_registry_key_reads_and_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n\n[[areas.installed]]\nid = \"timeline\"\nenabled = true\nversion = 2\n",
        )
        .unwrap();

        // A pre-V7 vault's registry is readable under the old key.
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                assert_eq!(
                    vault.config.routines.installed,
                    vec![InstalledRoutine::new("timeline".to_string(), true, 2)]
                );
            }
            other => panic!("expected valid vault, got {other:?}"),
        }

        // Any registry rewrite migrates it to [[routines.installed]].
        update_routines_registry(dir.path(), |_| {}).unwrap();
        let raw = fs::read_to_string(marker.join(VAULT_CONFIG_FILE)).unwrap();
        assert!(
            raw.contains("[[routines.installed]]"),
            "not migrated: {raw}"
        );
        assert!(!raw.contains("[[areas.installed]]"), "old key kept: {raw}");
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                assert_eq!(
                    vault.config.routines.installed,
                    vec![InstalledRoutine::new("timeline".to_string(), true, 2)]
                );
            }
            other => panic!("expected valid vault, got {other:?}"),
        }

        // Both keys present: the new key wins on duplicate ids.
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n\n[[areas.installed]]\nid = \"timeline\"\nenabled = true\nversion = 2\n\n\
             [[areas.installed]]\nid = \"legacy-only\"\nversion = 1\n\n\
             [[routines.installed]]\nid = \"timeline\"\nenabled = false\nversion = 3\n",
        )
        .unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                assert_eq!(
                    vault.config.routines.installed,
                    vec![
                        InstalledRoutine::new("timeline".to_string(), false, 3),
                        InstalledRoutine::new("legacy-only".to_string(), true, 1),
                    ]
                );
            }
            other => panic!("expected valid vault, got {other:?}"),
        }
    }

    #[test]
    fn day_planner_config_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n[day_planner]\nheading = \"Plan\"\nday_start = \"08:00\"\ndefault_duration_minutes = 45\n",
        )
        .unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                let config = &vault.config.day_planner;
                assert_eq!(config.heading, "Plan");
                assert_eq!(config.day_start, 480);
                // Unspecified keys keep their defaults.
                assert_eq!(config.day_end, 1440);
                assert_eq!(config.default_duration, 45);
                assert!(config.show_now_indicator);
            }
            other => panic!("expected valid vault, got {other:?}"),
        }
    }

    #[test]
    fn markdown_config_parses_and_defaults_to_concealed() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n[markdown]\nconceal = false\nemail_view = false\n",
        )
        .unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                assert!(!vault.config.markdown.conceal);
                assert!(!vault.config.markdown.email_view);
            }
            other => panic!("expected valid vault, got {other:?}"),
        }
        // An absent section means the defaults: concealed, email view on.
        assert!(VaultConfig::default().markdown.conceal);
        assert!(VaultConfig::default().markdown.email_view);
    }

    #[test]
    fn day_planner_sections_parse_with_lowercased_keys() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n[day_planner.sections]\nMeetings = 3\nCalendar = 5\n",
        )
        .unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                let sections = &vault.config.day_planner.sections;
                assert_eq!(sections.get("meetings"), Some(&3));
                assert_eq!(sections.get("calendar"), Some(&5));
                // The rest of the table keeps its defaults.
                assert_eq!(vault.config.day_planner.heading, "Day planner");
            }
            other => panic!("expected valid vault, got {other:?}"),
        }
    }

    #[test]
    fn day_planner_config_invalid_values_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n[day_planner]\nday_start = \"25:00\"\nday_end = \"05:00\"\n",
        )
        .unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                // 25:00 is invalid, and the surviving day_end (05:00) is
                // before the default day_start, so both fall back.
                assert_eq!(vault.config.day_planner.day_start, 360);
                assert_eq!(vault.config.day_planner.day_end, 1440);
            }
            other => panic!("expected valid vault, got {other:?}"),
        }
    }

    #[test]
    fn agent_config_parsing_and_update() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n[daily]\ndir = \"notes/daily\"\n",
        )
        .unwrap();

        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => assert_eq!(vault.config.agent.command, None),
            other => panic!("expected valid vault, got {other:?}"),
        }

        update_agent_command(dir.path(), Some("claude --dangerously".to_string())).unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                assert_eq!(
                    vault.config.agent.command,
                    Some("claude --dangerously".to_string())
                );
                // The rewrite preserves the rest of the config.
                assert_eq!(vault.config.daily.dir, "notes/daily");
            }
            other => panic!("expected valid vault, got {other:?}"),
        }

        update_agent_command(dir.path(), None).unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => assert_eq!(vault.config.agent.command, None),
            other => panic!("expected valid vault, got {other:?}"),
        }
        let raw = fs::read_to_string(marker.join(VAULT_CONFIG_FILE)).unwrap();
        assert!(
            !raw.contains("[agent]"),
            "cleared section reappeared: {raw}"
        );
    }

    #[test]
    fn blank_vault_agent_command_means_unset() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n[agent]\ncommand = \"  \"\n",
        )
        .unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => assert_eq!(vault.config.agent.command, None),
            other => panic!("expected valid vault, got {other:?}"),
        }
    }

    #[test]
    fn backlog_config_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n[backlog]\nfile = \"lists/backlog.md\"\n",
        )
        .unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                assert_eq!(vault.config.backlog.file, "lists/backlog.md");
                assert_eq!(vault.backlog_path(), dir.path().join("lists/backlog.md"));
            }
            other => panic!("expected valid vault, got {other:?}"),
        }

        // A blank value falls back to the default rather than pointing the
        // panel at the vault root itself.
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n[backlog]\nfile = \" \"\n",
        )
        .unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                assert_eq!(vault.config.backlog.file, "backlog.md");
            }
            other => panic!("expected valid vault, got {other:?}"),
        }
    }

    #[test]
    fn onboarding_registry_fields_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(marker.join(VAULT_CONFIG_FILE), DEFAULT_CONFIG_TOML).unwrap();

        let installed_at = chrono::Utc::now();
        update_routines_registry(dir.path(), |installed| {
            let entry = installed.first_mut().unwrap();
            entry.onboarding_installed_at = Some(installed_at);
            entry.onboarding_state = Some(OnboardingState::Pending);
        })
        .unwrap();

        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                let entry = &vault.config.routines.installed[0];
                assert_eq!(entry.onboarding_state, Some(OnboardingState::Pending));
                let roundtripped = entry.onboarding_installed_at.unwrap();
                // Serialized at second precision.
                assert_eq!(roundtripped.timestamp(), installed_at.timestamp());
            }
            other => panic!("expected valid vault, got {other:?}"),
        }

        // A pre-V5 entry (no onboarding fields) resolves to None.
        update_routines_registry(dir.path(), |installed| {
            installed.push(InstalledRoutine::new("finance".to_string(), true, 1));
        })
        .unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                let entry = &vault.config.routines.installed[1];
                assert_eq!(entry.onboarding_state, None);
                assert_eq!(entry.onboarding_installed_at, None);
            }
            other => panic!("expected valid vault, got {other:?}"),
        }
    }

    #[test]
    fn daily_note_date_inverts_note_path() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let VaultStatus::Valid(vault) = Vault::detect(dir.path()) else {
            panic!("expected valid vault");
        };
        let date = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
        let path = vault.note_path(NoteKind::Daily, date);
        assert_eq!(vault.daily_note_date(&path), Some(date));

        assert_eq!(
            vault.daily_note_date(&vault.note_path(NoteKind::Weekly, date)),
            None
        );
        assert_eq!(
            vault.daily_note_date(&dir.path().join("2026-07-20.md")),
            None
        );
        assert_eq!(
            vault.daily_note_date(&dir.path().join("daily/notes.md")),
            None
        );
        assert_eq!(
            vault.daily_note_date(&dir.path().join("daily/2026-07-20.txt")),
            None
        );
        assert_eq!(
            vault.daily_note_date(&dir.path().join("daily/sub/2026-07-20.md")),
            None
        );
    }

    #[test]
    fn daily_note_date_supports_nested_filename_formats() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n[daily]\nfilename = \"YYYY/MM/DD\"\n",
        )
        .unwrap();
        let VaultStatus::Valid(vault) = Vault::detect(dir.path()) else {
            panic!("expected valid vault");
        };
        let date = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
        let path = vault.note_path(NoteKind::Daily, date);
        assert_eq!(path, dir.path().join("daily/2026/07/20.md"));
        assert_eq!(vault.daily_note_date(&path), Some(date));
    }

    #[test]
    fn partial_config_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(VAULT_MARKER_DIR);
        fs::create_dir_all(&marker).unwrap();
        fs::write(
            marker.join(VAULT_CONFIG_FILE),
            "schema = 1\n[daily]\ndir = \"notes/daily\"\n",
        )
        .unwrap();
        match Vault::detect(dir.path()) {
            VaultStatus::Valid(vault) => {
                assert_eq!(vault.config.daily.dir, "notes/daily");
                assert_eq!(vault.config.daily.filename, "YYYY-MM-DD");
                // A config written before weekly notes existed still works.
                assert_eq!(vault.config.weekly, NotesConfig::weekly_default());
            }
            other => panic!("expected valid vault, got {other:?}"),
        }
    }
}
