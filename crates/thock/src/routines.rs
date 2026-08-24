//! Routines (V7 spec): life-domain bundles defined by a `routine.toml` file
//! living in the vault at `routines/<id>/routine.toml`. The app-shipped
//! catalog is one way such a file gets there; the user's own agent authoring
//! one is the other. Discovery, validation, activation, provenance (the hash
//! lockfile), and removal all treat both origins identically.

use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::notes::{NoteKind, TimelineEntry};
use crate::vault::{OnboardingState, VAULT_MARKER_DIR, Vault, write_if_missing};

pub const TIMELINE_ROUTINE_ID: &str = "timeline";
pub const INBOX_ROUTINE_ID: &str = "inbox";
/// Vault-visible home of Routine definitions: `routines/<id>/routine.toml`.
pub const ROUTINES_DIR: &str = "routines";
pub const ROUTINE_MANIFEST_FILE: &str = "routine.toml";
/// App-owned provenance (`files.lock`) under `.thock/`.
pub const INSTALLED_ROUTINES_DIR: &str = "routines";
const FILES_LOCK_FILE: &str = "files.lock";
/// Pre-V7 provenance locations, migrated by `reconcile_vault`.
const LEGACY_INSTALLED_AREAS_DIR: &str = "areas";
const LEGACY_MANIFEST_FILE: &str = "manifest.toml";

/// Core-owned materialized files (not part of any Routine): the
/// `routine.toml` format reference agents read, and the New Routine ritual.
pub const ROUTINES_REFERENCE_PATH: &str = "routines/ROUTINES.md";
pub const NEW_ROUTINE_SKILL_PATH: &str = "skills/thock/new-routine.md";
const ROUTINES_REFERENCE: &str = include_str!("../assets/routines/ROUTINES.md");
const NEW_ROUTINE_SKILL: &str = include_str!("../assets/skills/new-routine.md");

const TIMELINE_MANIFEST: &str = include_str!("../assets/routines/timeline/routine.toml");
const TIMELINE_DOC: &str = include_str!("../assets/routines/timeline/doc.md");
const TIMELINE_WEEK_REVIEW_SKILL: &str =
    include_str!("../assets/routines/timeline/skills/week-review.md");
const TIMELINE_WRAP_TODAY_SKILL: &str =
    include_str!("../assets/routines/timeline/skills/wrap-today.md");
const TIMELINE_WRAP_YESTERDAY_SKILL: &str =
    include_str!("../assets/routines/timeline/skills/wrap-yesterday.md");
const TIMELINE_ONBOARDING_SKILL: &str =
    include_str!("../assets/routines/timeline/skills/onboarding.md");
const TIMELINE_CONNECT_GOOGLE_WORKSPACE_SKILL: &str =
    include_str!("../assets/routines/timeline/skills/connect-google-workspace.md");
const TIMELINE_DASHBOARD_HTML: &str = include_str!("../assets/routines/timeline/assets/index.html");
const TIMELINE_DASHBOARD_SEED: &str =
    include_str!("../assets/routines/timeline/assets/data.seed.js");

const INBOX_MANIFEST: &str = include_str!("../assets/routines/inbox/routine.toml");
const INBOX_DOC: &str = include_str!("../assets/routines/inbox/doc.md");
const INBOX_TRIAGE_POLICY: &str = include_str!("../assets/routines/inbox/triage-policy.md");
const INBOX_TRIAGE_SKILL: &str = include_str!("../assets/routines/inbox/skills/triage-inbox.md");
const INBOX_SETUP_SKILL: &str = include_str!("../assets/routines/inbox/skills/setup-inbox.md");

/// The parsed shape of a `routine.toml` (V7 spec §6, schema 2). Parsing is
/// lenient-forward: unknown keys are collected and warned about, never fatal,
/// so agent typos produce a visible warning instead of a dead Routine.
#[derive(Debug, Deserialize)]
struct RoutineManifestContent {
    schema: Option<u32>,
    id: String,
    name: String,
    version: Option<u32>,
    summary: Option<String>,
    icon: Option<String>,
    doc: String,
    agent_doc: Option<String>,
    #[serde(default)]
    link: Vec<RoutineLinkContent>,
    #[serde(default)]
    scaffold: Vec<ScaffoldEntryContent>,
    #[serde(default)]
    skill: Vec<RoutineSkillContent>,
    /// Deprecated schema-1 alias for `[[link]] kind = "browser"`.
    #[serde(default)]
    surface: Vec<RoutineSurfaceContent>,
    onboarding: Option<OnboardingContent>,
    #[serde(flatten)]
    unknown: toml::Table,
}

#[derive(Debug, Deserialize)]
struct OnboardingContent {
    skill: String,
    #[serde(flatten)]
    unknown: toml::Table,
}

impl RoutineManifestContent {
    fn resolve(self) -> Result<RoutineManifest> {
        let mut warnings = Vec::new();
        collect_unknown_keys(&mut warnings, "", &self.unknown);
        let mut links = Vec::new();
        for link in self.link {
            links.push(link.resolve(&mut warnings));
        }
        for surface in self.surface {
            links.push(surface.resolve(&mut warnings));
        }
        let mut skills = Vec::new();
        for skill in self.skill {
            skills.push(skill.resolve(&mut warnings));
        }
        let mut scaffold = Vec::new();
        for entry in self.scaffold {
            scaffold.push(entry.resolve(&mut warnings)?);
        }
        if let Some(onboarding) = &self.onboarding {
            collect_unknown_keys(&mut warnings, "onboarding.", &onboarding.unknown);
        }
        validate_routine_id(&self.id)?;
        Ok(RoutineManifest {
            schema: self.schema.unwrap_or(1),
            id: self.id,
            name: self.name,
            version: self.version.unwrap_or(1),
            summary: self.summary.unwrap_or_default(),
            icon: self.icon,
            doc: self.doc,
            agent_doc: self.agent_doc,
            links,
            scaffold,
            skills,
            onboarding: self.onboarding.map(|onboarding| RoutineOnboarding {
                skill: onboarding.skill,
            }),
            warnings,
        })
    }
}

fn collect_unknown_keys(warnings: &mut Vec<String>, prefix: &str, unknown: &toml::Table) {
    for key in unknown.keys() {
        warnings.push(format!("unknown key {prefix}{key}"));
    }
}

/// Routine ids double as directory and registry names, so keep them to a
/// predictable, path-safe shape.
fn validate_routine_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !valid {
        bail!("Routine id {id:?} must be lowercase letters, digits, '-', or '_'");
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RoutineLinkContent {
    id: Option<String>,
    name: String,
    open: String,
    kind: Option<String>,
    icon: Option<String>,
    group: Option<String>,
    #[serde(default)]
    create: bool,
    #[serde(flatten)]
    unknown: toml::Table,
}

impl RoutineLinkContent {
    fn resolve(self, warnings: &mut Vec<String>) -> RoutineLink {
        collect_unknown_keys(warnings, "link.", &self.unknown);
        let kind = match self.kind.as_deref() {
            None | Some("editor") => LinkKind::Editor,
            Some("preview") => LinkKind::Preview,
            Some("browser") => LinkKind::Browser,
            Some(other) => {
                warnings.push(format!(
                    "unknown link kind {other:?} on {:?} (expected editor | preview | browser); \
                     opening in the editor",
                    self.name
                ));
                LinkKind::Editor
            }
        };
        RoutineLink {
            id: self
                .id
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| slugify(&self.name)),
            name: self.name,
            open: self.open,
            kind,
            icon: self.icon.filter(|icon| !icon.is_empty()),
            group: self.group.filter(|group| !group.is_empty()),
            create: self.create,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RoutineSurfaceContent {
    /// Schema 1's surface kind ("dashboard"); accepted and ignored — every
    /// surface maps to a browser link.
    #[allow(dead_code)]
    kind: Option<String>,
    name: String,
    open: String,
    #[serde(flatten)]
    unknown: toml::Table,
}

impl RoutineSurfaceContent {
    fn resolve(self, warnings: &mut Vec<String>) -> RoutineLink {
        collect_unknown_keys(warnings, "surface.", &self.unknown);
        RoutineLink {
            id: slugify(&self.name),
            name: self.name,
            open: self.open,
            kind: LinkKind::Browser,
            icon: None,
            group: None,
            create: false,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ScaffoldEntryContent {
    kind: String,
    path: String,
    source: Option<String>,
    #[serde(flatten)]
    unknown: toml::Table,
}

impl ScaffoldEntryContent {
    fn resolve(self, warnings: &mut Vec<String>) -> Result<ScaffoldEntry> {
        collect_unknown_keys(warnings, "scaffold.", &self.unknown);
        match self.kind.as_str() {
            "dir" => Ok(ScaffoldEntry::Dir { path: self.path }),
            "file" => Ok(ScaffoldEntry::File {
                path: self.path,
                source: self.source,
            }),
            other => bail!("unknown scaffold kind {other:?} (expected \"dir\" or \"file\")"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RoutineSkillContent {
    id: String,
    name: String,
    file: String,
    kind: Option<String>,
    summary: Option<String>,
    icon: Option<String>,
    #[serde(default)]
    reads: Vec<String>,
    #[serde(default)]
    writes: Vec<String>,
    #[serde(flatten)]
    unknown: toml::Table,
}

impl RoutineSkillContent {
    fn resolve(self, warnings: &mut Vec<String>) -> RoutineSkill {
        collect_unknown_keys(warnings, "skill.", &self.unknown);
        let kind = match self.kind.as_deref() {
            None | Some("ritual") => SkillKind::Ritual,
            Some("setup") => SkillKind::Setup,
            Some(other) => {
                warnings.push(format!(
                    "unknown skill kind {other:?} on {:?} (expected ritual | setup); \
                     listing it as a ritual",
                    self.name
                ));
                SkillKind::Ritual
            }
        };
        RoutineSkill {
            id: self.id,
            name: self.name,
            file: self.file,
            kind,
            summary: self.summary.unwrap_or_default(),
            icon: self.icon.filter(|icon| !icon.is_empty()),
            reads: self.reads,
            writes: self.writes,
        }
    }
}

fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutineManifest {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub version: u32,
    pub summary: String,
    /// Named icon from a small `IconName` subset; `None` or an unknown name
    /// falls back to `Blocks` (the panel owns the mapping).
    pub icon: Option<String>,
    /// Vault-relative path of the human explainer doc.
    pub doc: String,
    /// Vault-relative agent-facing conventions file (convention only; the
    /// app never injects it).
    pub agent_doc: Option<String>,
    /// The Routine's navigation rows, in order.
    pub links: Vec<RoutineLink>,
    pub scaffold: Vec<ScaffoldEntry>,
    pub skills: Vec<RoutineSkill>,
    /// The agentic-onboarding ritual (V5 §7.1), when the Routine ships one.
    pub onboarding: Option<RoutineOnboarding>,
    /// Non-fatal parse findings (unknown keys, unknown link kinds), surfaced
    /// as log lines so agent typos are visible rather than silently dropped.
    pub warnings: Vec<String>,
}

/// The `[onboarding]` manifest table: which materialized skill file the
/// auto-launched setup session reads. The file is also expected to appear as
/// a regular `[[skill]]` entry so materialization, removal, and the skills
/// list treat it like any other skill.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutineOnboarding {
    /// Vault-relative path of the onboarding skill file.
    pub skill: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// Open in a regular editor buffer.
    Editor,
    /// Open in rendered-markdown viewing mode.
    Preview,
    /// Open with the system handler (Zed has no web view, so HTML surfaces
    /// open in the default browser).
    Browser,
}

impl LinkKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::Preview => "preview",
            Self::Browser => "browser",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutineLink {
    /// Stable handle for keybindings (`thock::OpenLink`); defaults to
    /// the slugified name.
    pub id: String,
    pub name: String,
    /// Vault-relative path, possibly containing date templates (§6).
    pub open: String,
    pub kind: LinkKind,
    /// Named icon overriding the row default; `None` or an unknown name
    /// falls back to the default for the link's kind (the panel owns the
    /// mapping).
    pub icon: Option<String>,
    /// Optional group label. Grouped links are demoted out of the panel's
    /// primary list into a collapsed disclosure row carrying this label, so
    /// a Routine can keep rarely-used destinations without spending rows on
    /// them (V11 §4).
    pub group: Option<String>,
    /// Create the target from the matching note template when missing,
    /// like the core Today action.
    pub create: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScaffoldEntry {
    Dir {
        path: String,
    },
    /// `path` is the vault-relative destination. `source` is the asset path
    /// within a catalog package; vault-authored Routines use bare
    /// declarations (`source = None`) feeding the lockfile and removal.
    File {
        path: String,
        source: Option<String>,
    },
}

/// What a skill is _for_, which decides where the panel puts it (V11 §3).
/// A ritual recurs and earns a row; a setup step runs once and lives behind
/// the section's collapsed Setup row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SkillKind {
    #[default]
    Ritual,
    Setup,
}

impl SkillKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ritual => "ritual",
            Self::Setup => "setup",
        }
    }

    pub fn is_setup(self) -> bool {
        matches!(self, Self::Setup)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutineSkill {
    pub id: String,
    pub name: String,
    /// Vault-relative path of the skill file.
    pub file: String,
    /// Recurring ritual (the default) or one-time setup step.
    pub kind: SkillKind,
    pub summary: String,
    /// Named icon overriding the row default; `None` or an unknown name
    /// falls back to the skill default (the panel owns the mapping).
    pub icon: Option<String>,
    /// Declared read scope; surfaced only, not enforced.
    pub reads: Vec<String>,
    /// Declared write scope; surfaced only, not enforced.
    pub writes: Vec<String>,
}

pub fn parse_manifest(manifest_toml: &str) -> Result<RoutineManifest> {
    toml::from_str::<RoutineManifestContent>(manifest_toml)
        .context("parsing routine.toml")?
        .resolve()
}

/// Renders a manifest back to `routine.toml` (schema 2). Used when migration
/// has to point a definition at preserved (user-modified) file paths; the
/// output carries no comments.
fn render_manifest_toml(manifest: &RoutineManifest) -> Result<String> {
    fn is_false(value: &bool) -> bool {
        !value
    }

    #[derive(Serialize)]
    struct LinkOut<'a> {
        id: &'a str,
        name: &'a str,
        open: &'a str,
        kind: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        icon: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        group: Option<&'a str>,
        #[serde(skip_serializing_if = "is_false")]
        create: bool,
    }

    #[derive(Serialize)]
    struct ScaffoldOut<'a> {
        kind: &'a str,
        path: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<&'a str>,
    }

    #[derive(Serialize)]
    struct SkillOut<'a> {
        id: &'a str,
        name: &'a str,
        file: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        kind: Option<&'a str>,
        #[serde(skip_serializing_if = "str::is_empty")]
        summary: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        icon: Option<&'a str>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        reads: &'a Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        writes: &'a Vec<String>,
    }

    #[derive(Serialize)]
    struct OnboardingOut<'a> {
        skill: &'a str,
    }

    #[derive(Serialize)]
    struct ManifestOut<'a> {
        schema: u32,
        id: &'a str,
        name: &'a str,
        version: u32,
        #[serde(skip_serializing_if = "str::is_empty")]
        summary: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        icon: Option<&'a str>,
        doc: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_doc: Option<&'a str>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        link: Vec<LinkOut<'a>>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        scaffold: Vec<ScaffoldOut<'a>>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        skill: Vec<SkillOut<'a>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        onboarding: Option<OnboardingOut<'a>>,
    }

    let out = ManifestOut {
        schema: 2,
        id: &manifest.id,
        name: &manifest.name,
        version: manifest.version,
        summary: &manifest.summary,
        icon: manifest.icon.as_deref(),
        doc: &manifest.doc,
        agent_doc: manifest.agent_doc.as_deref(),
        link: manifest
            .links
            .iter()
            .map(|link| LinkOut {
                id: &link.id,
                name: &link.name,
                open: &link.open,
                kind: link.kind.as_str(),
                icon: link.icon.as_deref(),
                group: link.group.as_deref(),
                create: link.create,
            })
            .collect(),
        scaffold: manifest
            .scaffold
            .iter()
            .map(|entry| match entry {
                ScaffoldEntry::Dir { path } => ScaffoldOut {
                    kind: "dir",
                    path,
                    source: None,
                },
                ScaffoldEntry::File { path, source } => ScaffoldOut {
                    kind: "file",
                    path,
                    source: source.as_deref(),
                },
            })
            .collect(),
        skill: manifest
            .skills
            .iter()
            .map(|skill| SkillOut {
                id: &skill.id,
                name: &skill.name,
                file: &skill.file,
                // The default stays absent so a re-rendered definition keeps
                // reading like the hand-written one.
                kind: skill.kind.is_setup().then(|| skill.kind.as_str()),
                summary: &skill.summary,
                icon: skill.icon.as_deref(),
                reads: &skill.reads,
                writes: &skill.writes,
            })
            .collect(),
        onboarding: manifest
            .onboarding
            .as_ref()
            .map(|onboarding| OnboardingOut {
                skill: &onboarding.skill,
            }),
    };
    toml::to_string_pretty(&out).context("serializing routine.toml")
}

/// Vault-relative root under which Claude Code discovers project skills. Its
/// files are generated from the manifest, not shipped as static assets.
pub const CLAUDE_SKILLS_DIR: &str = ".claude/skills";

/// A file a Routine ships into the vault, pairing the vault-relative
/// destination with the asset path inside the catalog package it came from
/// (when it has one).
struct DeclaredFile {
    destination: String,
    source: Option<String>,
}

/// A `.claude/skills/<id>/SKILL.md` bridge generated from a skill's manifest
/// entry. Claude Code only discovers skills under `.claude/skills/`, so each
/// Routine skill gets a thin bridge there whose front matter Claude Code
/// reads and whose body points back to the canonical skill file — keeping one
/// source of truth for the ritual itself.
struct ClaudeBridge {
    destination: String,
    content: String,
}

fn yaml_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn claude_bridge_content(skill: &RoutineSkill) -> String {
    let mut content = format!(
        "---\nname: {name}\ndescription: {description}\ndisable-model-invocation: true\n---\n\n\
         This is a Thock Routine skill. Read and follow the full instructions in\n\
         `{file}` (relative to the vault root), then carry out the ritual it describes.\n\
         It appends to your notes and never rewrites what you wrote.\n",
        name = yaml_quote(&skill.name),
        description = yaml_quote(&skill.summary),
        file = skill.file,
    );
    if !skill.reads.is_empty() {
        content.push_str(&format!("\nReads: {}\n", skill.reads.join(", ")));
    }
    if !skill.writes.is_empty() {
        content.push_str(&format!("Writes: {}\n", skill.writes.join(", ")));
    }
    content
}

fn claude_bridge_files(manifest: &RoutineManifest) -> Vec<ClaudeBridge> {
    manifest
        .skills
        .iter()
        .map(|skill| ClaudeBridge {
            destination: format!("{CLAUDE_SKILLS_DIR}/{}/SKILL.md", skill.id),
            content: claude_bridge_content(skill),
        })
        .collect()
}

/// Every vault file the manifest declares as the Routine's own, in a stable
/// order: the definition itself, docs, skills, then scaffold files. This is
/// the set the lockfile hashes and removal is allowed to touch.
fn declared_files(manifest: &RoutineManifest) -> Vec<DeclaredFile> {
    let mut files = vec![
        DeclaredFile {
            destination: vault_manifest_rel_path(&manifest.id),
            source: None,
        },
        DeclaredFile {
            destination: manifest.doc.clone(),
            source: Some("doc.md".to_string()),
        },
    ];
    if let Some(agent_doc) = &manifest.agent_doc {
        files.push(DeclaredFile {
            destination: agent_doc.clone(),
            source: None,
        });
    }
    for skill in &manifest.skills {
        files.push(DeclaredFile {
            destination: skill.file.clone(),
            source: Some(format!("skills/{}.md", skill.id)),
        });
    }
    for entry in &manifest.scaffold {
        if let ScaffoldEntry::File { path, source } = entry {
            files.push(DeclaredFile {
                destination: path.clone(),
                source: source.clone(),
            });
        }
    }
    files
}

/// A Routine package shipped inside the Thock binary.
pub struct CatalogRoutine {
    pub manifest: RoutineManifest,
    manifest_toml: &'static str,
    assets: &'static [(&'static str, &'static str)],
}

impl CatalogRoutine {
    fn asset(&self, source: &str) -> Option<&'static str> {
        self.assets
            .iter()
            .find(|(path, _)| *path == source)
            .map(|(_, contents)| *contents)
    }
}

/// The app-shipped Routine catalog, in gallery order.
pub fn catalog() -> Result<Vec<CatalogRoutine>> {
    Ok(vec![
        CatalogRoutine {
            manifest: parse_manifest(TIMELINE_MANIFEST)
                .context("parsing the bundled Timeline Routine manifest")?,
            manifest_toml: TIMELINE_MANIFEST,
            assets: &[
                ("doc.md", TIMELINE_DOC),
                ("skills/week-review.md", TIMELINE_WEEK_REVIEW_SKILL),
                ("skills/wrap-today.md", TIMELINE_WRAP_TODAY_SKILL),
                ("skills/wrap-yesterday.md", TIMELINE_WRAP_YESTERDAY_SKILL),
                ("skills/onboarding.md", TIMELINE_ONBOARDING_SKILL),
                (
                    "skills/connect-google-workspace.md",
                    TIMELINE_CONNECT_GOOGLE_WORKSPACE_SKILL,
                ),
                ("assets/index.html", TIMELINE_DASHBOARD_HTML),
                ("assets/data.seed.js", TIMELINE_DASHBOARD_SEED),
            ],
        },
        CatalogRoutine {
            manifest: parse_manifest(INBOX_MANIFEST)
                .context("parsing the bundled Inbox Routine manifest")?,
            manifest_toml: INBOX_MANIFEST,
            assets: &[
                ("doc.md", INBOX_DOC),
                ("triage-policy.md", INBOX_TRIAGE_POLICY),
                ("skills/triage-inbox.md", INBOX_TRIAGE_SKILL),
                ("skills/setup-inbox.md", INBOX_SETUP_SKILL),
            ],
        },
    ])
}

pub fn catalog_routine(routine_id: &str) -> Result<Option<CatalogRoutine>> {
    Ok(catalog()?
        .into_iter()
        .find(|routine| routine.manifest.id == routine_id))
}

/// Joins a manifest-declared vault-relative path onto the vault root,
/// rejecting absolute paths and `..` so a manifest can never reach outside
/// the vault.
pub fn vault_file_path(vault_root: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    let plain = !relative_path.as_os_str().is_empty()
        && relative_path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !plain {
        bail!("Routine path {relative:?} must be a plain vault-relative path");
    }
    Ok(vault_root.join(relative_path))
}

fn installed_routine_dir(vault_root: &Path, routine_id: &str) -> PathBuf {
    vault_root
        .join(VAULT_MARKER_DIR)
        .join(INSTALLED_ROUTINES_DIR)
        .join(routine_id)
}

fn files_lock_path(vault_root: &Path, routine_id: &str) -> PathBuf {
    installed_routine_dir(vault_root, routine_id).join(FILES_LOCK_FILE)
}

fn legacy_installed_manifest_path(vault_root: &Path, routine_id: &str) -> PathBuf {
    installed_routine_dir(vault_root, routine_id).join(LEGACY_MANIFEST_FILE)
}

fn vault_manifest_rel_path(routine_id: &str) -> String {
    format!("{ROUTINES_DIR}/{routine_id}/{ROUTINE_MANIFEST_FILE}")
}

pub fn vault_manifest_path(vault_root: &Path, routine_id: &str) -> PathBuf {
    vault_root
        .join(ROUTINES_DIR)
        .join(routine_id)
        .join(ROUTINE_MANIFEST_FILE)
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// Loads a Routine's definition from `routines/<id>/routine.toml`, if
/// present. The manifest's `id` must match the directory name.
pub fn load_vault_manifest(vault_root: &Path, routine_id: &str) -> Result<Option<RoutineManifest>> {
    let manifest_path = vault_manifest_path(vault_root, routine_id);
    let Some(manifest_toml) = read_optional(&manifest_path)? else {
        return Ok(None);
    };
    let manifest = parse_manifest(&manifest_toml)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    if manifest.id != routine_id {
        bail!(
            "routine.toml id {:?} doesn't match its directory {routine_id:?}",
            manifest.id
        );
    }
    Ok(Some(manifest))
}

/// Loads the pre-V7 app-owned provenance manifest, if one is still around.
fn load_legacy_installed_manifest(
    vault_root: &Path,
    routine_id: &str,
) -> Result<Option<RoutineManifest>> {
    for path in [
        legacy_installed_manifest_path(vault_root, routine_id),
        vault_root
            .join(VAULT_MARKER_DIR)
            .join(LEGACY_INSTALLED_AREAS_DIR)
            .join(routine_id)
            .join(LEGACY_MANIFEST_FILE),
    ] {
        if let Some(manifest_toml) = read_optional(&path)? {
            return parse_manifest(&manifest_toml)
                .with_context(|| format!("parsing {}", path.display()))
                .map(Some);
        }
    }
    Ok(None)
}

// --- The hash lockfile (§5.3) ---

/// `.thock/routines/<id>/files.lock`: the content hash of every declared
/// file at install/activation time. Removal compares current content against
/// these hashes, so "preserve modified files" works identically for catalog
/// and vault-authored Routines.
#[derive(Debug, Default, Serialize, Deserialize)]
struct FilesLock {
    #[serde(default)]
    files: BTreeMap<String, String>,
}

fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn load_files_lock(vault_root: &Path, routine_id: &str) -> Result<Option<FilesLock>> {
    let path = files_lock_path(vault_root, routine_id);
    let Some(raw) = read_optional(&path)? else {
        return Ok(None);
    };
    toml::from_str(&raw)
        .with_context(|| format!("parsing {}", path.display()))
        .map(Some)
}

fn write_files_lock(vault_root: &Path, routine_id: &str, lock: &FilesLock) -> Result<()> {
    let path = files_lock_path(vault_root, routine_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let serialized = toml::to_string_pretty(lock).context("serializing files.lock")?;
    fs::write(&path, serialized).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// The lockfile for a catalog install: hashes of the *shipped* contents
/// (assets and the packaged manifest), not whatever is on disk — so files the
/// user edited before or after install always classify as modified.
fn catalog_files_lock(routine: &CatalogRoutine) -> FilesLock {
    let mut lock = FilesLock::default();
    for file in declared_files(&routine.manifest) {
        let shipped = if file.destination == vault_manifest_rel_path(&routine.manifest.id) {
            Some(routine.manifest_toml)
        } else {
            file.source
                .as_deref()
                .and_then(|source| routine.asset(source))
        };
        if let Some(shipped) = shipped {
            lock.files
                .insert(file.destination, content_hash(shipped.as_bytes()));
        }
    }
    lock
}

/// The lockfile for a vault-authored activation: hashes of the files as they
/// exist right now — the agent-authored content is the pristine baseline.
fn on_disk_files_lock(vault_root: &Path, manifest: &RoutineManifest) -> Result<FilesLock> {
    let mut lock = FilesLock::default();
    for file in declared_files(manifest) {
        let path = vault_file_path(vault_root, &file.destination)?;
        match fs::read(&path) {
            Ok(contents) => {
                lock.files.insert(file.destination, content_hash(&contents));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        }
    }
    Ok(lock)
}

// --- Discovery & activation (§5.2, §7.1) ---

/// A `routines/<id>/routine.toml` found in the vault. Invalid definitions are
/// carried as errors so the picker can render them as visible error rows.
#[derive(Debug)]
pub struct DiscoveredRoutine {
    /// The directory name (the would-be Routine id).
    pub id: String,
    pub manifest: Result<RoutineManifest, String>,
}

/// Scans `routines/*/routine.toml` (one directory level). Blocking I/O.
pub fn discover_routines(vault_root: &Path) -> Vec<DiscoveredRoutine> {
    let routines_dir = vault_root.join(ROUTINES_DIR);
    let Ok(entries) = fs::read_dir(&routines_dir) else {
        return Vec::new();
    };
    let mut discovered = Vec::new();
    for entry in entries.flatten() {
        let Ok(id) = entry.file_name().into_string() else {
            continue;
        };
        if !entry.path().join(ROUTINE_MANIFEST_FILE).is_file() {
            continue;
        }
        let manifest = load_vault_manifest(vault_root, &id)
            .map_err(|error| format!("{error:#}"))
            .and_then(|manifest| {
                manifest.ok_or_else(|| "routine.toml disappeared mid-scan".to_string())
            });
        discovered.push(DiscoveredRoutine { id, manifest });
    }
    discovered.sort_by(|a, b| a.id.cmp(&b.id));
    discovered
}

/// A cheap fingerprint of everything the panel renders from outside the
/// vault config: the discovered manifests and the pending ready markers.
/// `refresh_vault_status` folds this into its compared snapshot so a new or
/// edited `routine.toml` re-renders without a registry write (§9 trap 2).
pub fn refresh_fingerprint(vault_root: &Path) -> Vec<(String, String)> {
    let mut fingerprint = Vec::new();
    let routines_dir = vault_root.join(ROUTINES_DIR);
    if let Ok(entries) = fs::read_dir(&routines_dir) {
        for entry in entries.flatten() {
            let Ok(id) = entry.file_name().into_string() else {
                continue;
            };
            if let Ok(contents) = fs::read(entry.path().join(ROUTINE_MANIFEST_FILE)) {
                fingerprint.push((id, content_hash(&contents)));
            }
        }
    }
    for marker in pending_ready_markers(vault_root) {
        fingerprint.push((format!("ready:{marker}"), String::new()));
    }
    fingerprint.sort();
    fingerprint
}

/// Activates a discovered (vault-authored or hand-copied) Routine: validate,
/// record the hash lockfile, create declared scaffold dirs, generate the
/// Claude Code bridges, and register it enabled. Registration happens last so
/// a failed activation never leaves a registered-but-broken Routine behind.
/// Blocking I/O — call from a background thread.
pub fn activate_routine(vault_root: &Path, routine_id: &str) -> Result<RoutineManifest> {
    let manifest = load_vault_manifest(vault_root, routine_id)?
        .with_context(|| format!("no routine.toml for {routine_id:?} in this vault"))?;
    for file in declared_files(&manifest) {
        vault_file_path(vault_root, &file.destination)?;
    }
    for entry in &manifest.scaffold {
        if let ScaffoldEntry::Dir { path } = entry {
            let dir = vault_file_path(vault_root, path)?;
            fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        }
    }
    write_files_lock(
        vault_root,
        routine_id,
        &on_disk_files_lock(vault_root, &manifest)?,
    )?;
    for bridge in claude_bridge_files(&manifest) {
        write_if_missing(
            &vault_file_path(vault_root, &bridge.destination)?,
            &bridge.content,
        )?;
    }
    let has_onboarding = manifest.onboarding.is_some();
    let version = manifest.version;
    crate::vault::update_routines_registry(vault_root, |installed| {
        if let Some(entry) = installed.iter_mut().find(|entry| entry.id == routine_id) {
            entry.enabled = true;
            entry.version = version;
        } else {
            let mut entry =
                crate::vault::InstalledRoutine::new(routine_id.to_string(), true, version);
            if has_onboarding {
                entry.onboarding_state = Some(OnboardingState::Pending);
                entry.onboarding_installed_at = Some(Utc::now());
            }
            installed.push(entry);
        }
    })?;
    clear_ready_marker(vault_root, routine_id);
    Ok(manifest)
}

// --- The New Routine ritual's ready marker (§7.2) ---

fn ready_marker_dir(vault_root: &Path) -> PathBuf {
    vault_root
        .join(VAULT_MARKER_DIR)
        .join("state")
        .join("routine-ready")
}

pub fn ready_marker_path(vault_root: &Path, routine_id: &str) -> PathBuf {
    ready_marker_dir(vault_root).join(routine_id)
}

/// Routine ids whose authoring ritual reported completion and which await
/// activation. Blocking I/O.
pub fn pending_ready_markers(vault_root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(ready_marker_dir(vault_root)) else {
        return Vec::new();
    };
    let mut markers: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    markers.sort();
    markers
}

fn clear_ready_marker(vault_root: &Path, routine_id: &str) {
    let marker = ready_marker_path(vault_root, routine_id);
    match fs::remove_file(&marker) {
        Ok(()) => prune_empty_parents(vault_root, &marker),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => log::warn!(
            "Thock: couldn't remove the ready marker {}: {error}",
            marker.display()
        ),
    }
}

// --- Catalog install, reconcile (§7.3) ---

/// Writes a catalog Routine's editable files into the vault (create-if-
/// missing, never clobbering), including its live `routine.toml` definition,
/// and records the hash lockfile under `.thock/routines/<id>/`.
/// Idempotent; missing files are re-materialized. When the catalog ships a
/// newer definition, an *unmodified* `routine.toml` is upgraded in place;
/// a user-edited one keeps its edits and misses the upgrade, with a log line
/// (V7 decision 8). Blocking I/O — call from a background thread.
pub fn materialize_routine(vault_root: &Path, routine: &CatalogRoutine) -> Result<()> {
    for entry in &routine.manifest.scaffold {
        if let ScaffoldEntry::Dir { path } = entry {
            let dir = vault_file_path(vault_root, path)?;
            fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        }
    }
    for file in declared_files(&routine.manifest) {
        let Some(source) = &file.source else {
            continue;
        };
        let contents = routine.asset(source).with_context(|| {
            format!(
                "the {} Routine package has no asset {source:?}",
                routine.manifest.id
            )
        })?;
        write_if_missing(&vault_file_path(vault_root, &file.destination)?, contents)?;
    }
    for bridge in claude_bridge_files(&routine.manifest) {
        write_if_missing(
            &vault_file_path(vault_root, &bridge.destination)?,
            &bridge.content,
        )?;
    }

    let routine_id = &routine.manifest.id;
    let manifest_path = vault_manifest_path(vault_root, routine_id);
    let existing = read_optional(&manifest_path)?;
    let previous_lock = load_files_lock(vault_root, routine_id)?;
    match existing {
        None => {
            write_if_missing(&manifest_path, routine.manifest_toml)?;
        }
        Some(existing) if existing == routine.manifest_toml => {}
        Some(existing) => {
            let recorded_hash = previous_lock
                .as_ref()
                .and_then(|lock| lock.files.get(&vault_manifest_rel_path(routine_id)));
            if recorded_hash == Some(&content_hash(existing.as_bytes())) {
                fs::write(&manifest_path, routine.manifest_toml)
                    .with_context(|| format!("writing {}", manifest_path.display()))?;
            } else {
                log::info!(
                    "Thock: routines/{routine_id}/routine.toml was modified by the user; \
                     keeping it and skipping the packaged v{} update",
                    routine.manifest.version
                );
                return Ok(());
            }
        }
    }
    write_files_lock(vault_root, routine_id, &catalog_files_lock(routine))?;
    Ok(())
}

/// Materializes a catalog Routine and registers it (enabled) in the vault
/// config. Registration happens last, so a failed install never leaves a
/// registered-but-missing Routine behind. For Routines that ship an
/// onboarding ritual, only a genuinely new registry entry (first install, or
/// a re-add after full removal) enters the onboarding flow as `pending`:
/// re-enabling, reinstalling files, and pre-V5/scaffolded entries never
/// (re)trigger setup (V5 locked decision 14). Blocking I/O — call from a
/// background thread.
pub fn install_routine(vault_root: &Path, routine_id: &str) -> Result<()> {
    let routine = catalog_routine(routine_id)?
        .with_context(|| format!("no Routine {routine_id:?} in the catalog"))?;
    materialize_routine(vault_root, &routine)?;
    let has_onboarding = routine.manifest.onboarding.is_some();
    crate::vault::update_routines_registry(vault_root, |installed| {
        if let Some(entry) = installed.iter_mut().find(|entry| entry.id == routine_id) {
            entry.enabled = true;
            entry.version = routine.manifest.version;
        } else {
            let mut entry = crate::vault::InstalledRoutine::new(
                routine_id.to_string(),
                true,
                routine.manifest.version,
            );
            if has_onboarding {
                entry.onboarding_state = Some(OnboardingState::Pending);
                entry.onboarding_installed_at = Some(Utc::now());
            }
            installed.push(entry);
        }
    })
}

/// Where an agent reports Routine setup as done (V5 §5.4): the filesystem is
/// the one channel the app and the terminal-hosted agent share. The path
/// contract is spelled out in each Routine's onboarding skill file.
pub fn onboarding_marker_path(vault_root: &Path, routine_id: &str) -> PathBuf {
    vault_root
        .join(VAULT_MARKER_DIR)
        .join("state")
        .join("onboarded")
        .join(routine_id)
}

/// How long a `pending` onboarding may sit without a marker before the app
/// re-prompts once and goes quiet (V5 §7.4).
pub fn onboarding_expiry() -> chrono::Duration {
    chrono::Duration::hours(24)
}

/// What the app should do for a Routine's onboarding right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OnboardingCheck {
    Nothing,
    /// The done marker appeared: persist `onboarded`, clear the badge, open
    /// the capabilities tour.
    MarkOnboarded,
    /// 24 h passed with no marker: persist `expired`, then re-prompt once.
    PromptExpiry,
}

/// The onboarding state machine (V5 §7.4), pure for testability:
/// pending → onboarded | expired, and expired → onboarded (a late "Set up
/// with AI" run can still finish). `state == None` is a pre-V5 or scaffolded
/// install — treated as expired from the start, so it never badges or
/// prompts, but a marker still completes it.
pub fn check_onboarding(
    state: Option<OnboardingState>,
    installed_at: Option<DateTime<Utc>>,
    marker_exists: bool,
    now: DateTime<Utc>,
) -> OnboardingCheck {
    match state {
        Some(OnboardingState::Onboarded) => OnboardingCheck::Nothing,
        Some(OnboardingState::Pending) => {
            if marker_exists {
                OnboardingCheck::MarkOnboarded
            } else if installed_at
                .is_none_or(|installed_at| now - installed_at > onboarding_expiry())
            {
                OnboardingCheck::PromptExpiry
            } else {
                OnboardingCheck::Nothing
            }
        }
        Some(OnboardingState::Expired) | None => {
            if marker_exists {
                OnboardingCheck::MarkOnboarded
            } else {
                OnboardingCheck::Nothing
            }
        }
    }
}

/// Persists an onboarding state transition. Returns whether anything changed,
/// so callers fire one-shot effects (the tour, the expiry re-prompt) exactly
/// once even when checks race. Blocking I/O — call from a background thread.
pub fn set_onboarding_state(
    vault_root: &Path,
    routine_id: &str,
    state: OnboardingState,
) -> Result<bool> {
    let mut changed = false;
    crate::vault::update_routines_registry(vault_root, |installed| {
        if let Some(entry) = installed.iter_mut().find(|entry| entry.id == routine_id)
            && entry.onboarding_state != Some(state)
        {
            entry.onboarding_state = Some(state);
            changed = true;
        }
    })?;
    Ok(changed)
}

/// Writes the core-owned Routine authoring files (the format reference and
/// the New Routine ritual) — create-if-missing, like the rest of the vault
/// scaffold.
pub fn materialize_core_files(vault_root: &Path) -> Result<()> {
    write_if_missing(
        &vault_root.join(ROUTINES_REFERENCE_PATH),
        ROUTINES_REFERENCE,
    )?;
    write_if_missing(&vault_root.join(NEW_ROUTINE_SKILL_PATH), NEW_ROUTINE_SKILL)?;
    Ok(())
}

/// The per-vault-open reconcile pass: migrates pre-V7 layouts (§2), keeps the
/// core authoring files present, and re-materializes every enabled catalog
/// Routine (create-if-missing) so a vault opened after an app update gains
/// newly shipped files without a manual reinstall. Vault-authored Routines
/// have no package to restore from and are left alone. Idempotent and never
/// clobbers user edits. Blocking I/O — call from a background thread.
pub fn reconcile_vault(vault: &Vault) -> Result<()> {
    migrate_pre_v7(vault)?;
    materialize_core_files(&vault.root)?;
    for entry in &vault.config.routines.installed {
        if !entry.enabled {
            continue;
        }
        if let Some(routine) = catalog_routine(&entry.id)? {
            materialize_routine(&vault.root, &routine)?;
        }
    }
    Ok(())
}

// --- Pre-V7 migration (§2) ---

/// One-time, conservative migration of a pre-V7 vault:
/// - `.thock/areas/<id>/` moves to `.thock/routines/<id>/`
///   (app-owned provenance, moved silently);
/// - registered Routines without a `routine.toml` get one written from the
///   catalog (or their legacy installed manifest), with shipped files moved
///   to the `routines/<id>/` layout only when they are unmodified —
///   user-edited files stay put and the written definition points at them;
/// - the registry key migrates from `[[areas.installed]]` to
///   `[[routines.installed]]` on the next registry write (the caller's
///   config rewrite), which older builds can no longer read (decision 4).
fn migrate_pre_v7(vault: &Vault) -> Result<()> {
    let root = &vault.root;
    let legacy_dir = root.join(VAULT_MARKER_DIR).join(LEGACY_INSTALLED_AREAS_DIR);
    if legacy_dir.is_dir() {
        let new_dir = root.join(VAULT_MARKER_DIR).join(INSTALLED_ROUTINES_DIR);
        fs::create_dir_all(&new_dir).with_context(|| format!("creating {}", new_dir.display()))?;
        for entry in fs::read_dir(&legacy_dir)
            .with_context(|| format!("reading {}", legacy_dir.display()))?
            .flatten()
        {
            let destination = new_dir.join(entry.file_name());
            if !destination.exists() {
                fs::rename(entry.path(), &destination).with_context(|| {
                    format!(
                        "moving {} to {}",
                        entry.path().display(),
                        destination.display()
                    )
                })?;
            }
        }
        if let Err(error) = fs::remove_dir(&legacy_dir) {
            log::debug!(
                "Thock: left {} behind after migration: {error}",
                legacy_dir.display()
            );
        }
    }

    let mut migrated = Vec::new();
    for entry in &vault.config.routines.installed {
        if vault_manifest_path(root, &entry.id).is_file() {
            continue;
        }
        let Some(legacy_manifest) = load_legacy_installed_manifest(root, &entry.id)? else {
            continue;
        };
        let version = migrate_installed_routine(root, &entry.id, &legacy_manifest)?;
        migrated.push((entry.id.clone(), version));
    }

    // The registry-key rename is committed by any registry rewrite; force one
    // here so a migrated vault is consistently on the new key, with each
    // entry stamped with the version its migrated definition carries.
    if !migrated.is_empty() {
        crate::vault::update_routines_registry(root, |installed| {
            for (id, version) in &migrated {
                if let Some(entry) = installed.iter_mut().find(|entry| entry.id == *id) {
                    entry.version = *version;
                }
            }
        })?;
    }
    Ok(())
}

/// Returns the version of the definition it wrote.
fn migrate_installed_routine(
    root: &Path,
    routine_id: &str,
    legacy: &RoutineManifest,
) -> Result<u32> {
    let catalog_routine = catalog_routine(routine_id)?;
    let (mut manifest, packaged_toml, lock) = match &catalog_routine {
        Some(routine) => (
            routine.manifest.clone(),
            Some(routine.manifest_toml),
            catalog_files_lock(routine),
        ),
        // A registered non-catalog bundle: keep its declared layout as-is.
        None => (legacy.clone(), None, FilesLock::default()),
    };
    let mut lock = lock;
    let mut kept_in_place = false;

    if let Some(routine) = &catalog_routine {
        // Pair the legacy destinations with the new layout's by asset source,
        // then move unmodified files and leave edited ones where they are
        // (the definition below points at whichever path survived).
        let legacy_by_source: Vec<(String, String)> = declared_files(legacy)
            .into_iter()
            .filter_map(|file| Some((file.source?, file.destination)))
            .collect();
        let mut relocate = |source: &str, new_destination: &mut String| -> Result<()> {
            let Some((_, old_rel)) = legacy_by_source
                .iter()
                .find(|(legacy_source, _)| legacy_source == source)
            else {
                return Ok(());
            };
            if old_rel == new_destination.as_str() {
                return Ok(());
            }
            let old_path = vault_file_path(root, old_rel)?;
            let Some(current) = read_optional(&old_path)? else {
                return Ok(());
            };
            let unmodified = routine.asset(source).is_some_and(|asset| current == asset);
            let new_path = vault_file_path(root, new_destination)?;
            if unmodified && !new_path.exists() {
                if let Some(parent) = new_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                fs::rename(&old_path, &new_path).with_context(|| {
                    format!("moving {} to {}", old_path.display(), new_path.display())
                })?;
                prune_empty_parents(root, &old_path);
            } else {
                // Never-clobber: the definition keeps pointing at the file
                // the user (or a collision) left in place. Its lockfile entry
                // moves with it so removal still classifies it correctly.
                if let Some(hash) = lock.files.remove(new_destination.as_str()) {
                    lock.files.insert(old_rel.clone(), hash);
                }
                *new_destination = old_rel.clone();
                kept_in_place = true;
                log::info!(
                    "Thock: {old_rel} was modified (or its new home exists); the migrated \
                     {routine_id} Routine keeps it in place"
                );
            }
            Ok(())
        };

        let mut doc = manifest.doc.clone();
        relocate("doc.md", &mut doc)?;
        manifest.doc = doc;
        let mut skills = manifest.skills.clone();
        for skill in &mut skills {
            let source = format!("skills/{}.md", skill.id);
            let mut file = skill.file.clone();
            relocate(&source, &mut file)?;
            if let Some(onboarding) = &mut manifest.onboarding
                && onboarding.skill == skill.file
            {
                onboarding.skill = file.clone();
            }
            skill.file = file;
        }
        manifest.skills = skills;
    }

    // Refresh the generated bridges: one that still matches its legacy
    // generated content is app-owned and follows the new skill paths; an
    // edited one is the user's and stays.
    let legacy_bridges = claude_bridge_files(legacy);
    for bridge in claude_bridge_files(&manifest) {
        let path = vault_file_path(root, &bridge.destination)?;
        let legacy_content = legacy_bridges
            .iter()
            .find(|legacy_bridge| legacy_bridge.destination == bridge.destination)
            .map(|legacy_bridge| legacy_bridge.content.as_str());
        match read_optional(&path)? {
            None => write_if_missing(&path, &bridge.content)?,
            Some(current) if Some(current.as_str()) == legacy_content => {
                fs::write(&path, &bridge.content)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
            Some(_) => {}
        }
    }

    let manifest_toml = match (packaged_toml, kept_in_place) {
        (Some(packaged), false) => packaged.to_string(),
        _ => render_manifest_toml(&manifest)?,
    };
    let manifest_path = vault_manifest_path(root, routine_id);
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&manifest_path, &manifest_toml)
        .with_context(|| format!("writing {}", manifest_path.display()))?;
    if kept_in_place {
        // The rendered definition points at preserved files; recording no
        // hash for it makes later reconciles treat it as user-modified, so
        // a packaged upgrade can never clobber the preserved paths.
        lock.files.remove(&vault_manifest_rel_path(routine_id));
    } else {
        lock.files.insert(
            vault_manifest_rel_path(routine_id),
            content_hash(manifest_toml.as_bytes()),
        );
    }
    if catalog_routine.is_none() {
        // No shipped baseline exists; record the current content so removal
        // can at least delete what stays untouched from here on.
        lock = on_disk_files_lock(root, &manifest)?;
    }
    write_files_lock(root, routine_id, &lock)?;

    let legacy_manifest_path = legacy_installed_manifest_path(root, routine_id);
    match fs::remove_file(&legacy_manifest_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => log::warn!(
            "Thock: couldn't remove {}: {error}",
            legacy_manifest_path.display()
        ),
    }
    Ok(manifest.version)
}

// --- Registry state & loading (§7.1) ---

/// Disables a Routine in the registry without touching any files.
pub fn deactivate_routine(vault_root: &Path, routine_id: &str) -> Result<()> {
    crate::vault::update_routines_registry(vault_root, |installed| {
        if let Some(entry) = installed.iter_mut().find(|entry| entry.id == routine_id) {
            entry.enabled = false;
        }
    })
}

/// An enabled registry entry as the panel renders it: its live manifest, or
/// a visible error when the definition is missing or invalid — never a
/// silent skip (§3 criterion 3).
#[derive(Debug)]
pub enum RoutineLoad {
    Loaded(RoutineManifest),
    Invalid { id: String, error: String },
}

impl RoutineLoad {
    pub fn id(&self) -> &str {
        match self {
            Self::Loaded(manifest) => &manifest.id,
            Self::Invalid { id, .. } => id,
        }
    }
}

/// The vault's enabled Routines, in registry order, loaded from their
/// on-disk `routine.toml` (catalog fallback only for pre-V7 entries that
/// haven't been migrated yet).
pub fn enabled_routines(vault: &Vault) -> Vec<RoutineLoad> {
    let mut routines = Vec::new();
    for entry in &vault.config.routines.installed {
        if !entry.enabled {
            continue;
        }
        let load = match load_vault_manifest(&vault.root, &entry.id) {
            Ok(Some(manifest)) => {
                for warning in &manifest.warnings {
                    log::warn!("Thock: routines/{}/routine.toml: {warning}", entry.id);
                }
                RoutineLoad::Loaded(manifest)
            }
            Ok(None) => match load_legacy_installed_manifest(&vault.root, &entry.id) {
                Ok(Some(manifest)) => RoutineLoad::Loaded(manifest),
                Ok(None) => match catalog_routine(&entry.id) {
                    Ok(Some(routine)) => RoutineLoad::Loaded(routine.manifest),
                    Ok(None) => RoutineLoad::Invalid {
                        id: entry.id.clone(),
                        error: format!(
                            "routines/{}/routine.toml is missing and the Routine is not in \
                             the catalog",
                            entry.id
                        ),
                    },
                    Err(error) => RoutineLoad::Invalid {
                        id: entry.id.clone(),
                        error: format!("{error:#}"),
                    },
                },
                Err(error) => RoutineLoad::Invalid {
                    id: entry.id.clone(),
                    error: format!("{error:#}"),
                },
            },
            Err(error) => RoutineLoad::Invalid {
                id: entry.id.clone(),
                error: format!("{error:#}"),
            },
        };
        routines.push(load);
    }
    routines
}

/// The manifests of the enabled Routines that loaded cleanly.
pub fn enabled_routine_manifests(vault: &Vault) -> Vec<RoutineManifest> {
    enabled_routines(vault)
        .into_iter()
        .filter_map(|load| match load {
            RoutineLoad::Loaded(manifest) => Some(manifest),
            RoutineLoad::Invalid { .. } => None,
        })
        .collect()
}

// --- Link resolution (§6 date templates) ---

/// A link's `open` value with its date templates resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLink {
    pub relative_path: String,
    /// The note the path stands for, when the template used a date token —
    /// what create-if-missing creates from the matching template.
    pub note: Option<(NoteKind, NaiveDate)>,
}

/// Resolves the closed template vocabulary — {today} {yesterday} {tomorrow}
/// {this_week} {last_week} — against the vault's `[daily]`/`[weekly]`
/// filename config. Unknown tokens are errors; no format strings come from
/// manifests.
pub fn resolve_link(vault: &Vault, open: &str, today: NaiveDate) -> Result<ResolvedLink> {
    let mut resolved = String::with_capacity(open.len());
    let mut note = None;
    let mut rest = open;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            bail!("unclosed template token in {open:?}");
        };
        resolved.push_str(&rest[..start]);
        let token = &rest[start + 1..start + end];
        let entry = match token {
            "today" => TimelineEntry::Today,
            "yesterday" => TimelineEntry::Yesterday,
            "tomorrow" => TimelineEntry::Tomorrow,
            "this_week" => TimelineEntry::ThisWeek,
            "last_week" => TimelineEntry::LastWeek,
            other => bail!("unknown template token {{{other}}} in {open:?}"),
        };
        let (kind, date) = entry
            .resolve(today)
            .with_context(|| format!("{{{token}}} has no valid date for {today}"))?;
        resolved.push_str(&crate::notes::format_date(
            date,
            &vault.notes_config(kind).filename,
        ));
        if note.is_none() {
            note = Some((kind, date));
        }
        rest = &rest[start + end + 1..];
    }
    resolved.push_str(rest);
    Ok(ResolvedLink {
        relative_path: resolved,
        note,
    })
}

/// Ensures a `create = true` link's target exists: date-template links are
/// created from their note kind's template (like the core Today action);
/// plain links get an empty file. Returns the absolute path. Blocking I/O.
pub fn ensure_link_target(
    vault: &Vault,
    create: bool,
    resolved: &ResolvedLink,
    time: NaiveTime,
) -> Result<PathBuf> {
    let path = vault_file_path(&vault.root, &resolved.relative_path)?;
    if !create || path.exists() {
        return Ok(path);
    }
    match resolved.note {
        Some((kind, date)) => {
            crate::notes::ensure_note_at(vault, kind, date, time, &path)?;
        }
        None => write_if_missing(&path, "")?,
    }
    Ok(path)
}

// --- Removal (§7.3) ---

/// What removing a Routine's files would do, computed before the user
/// confirms.
#[derive(Debug, PartialEq)]
pub struct RemovalPlan {
    pub routine_name: String,
    /// The Routine has no catalog package — it was authored in this vault
    /// (provenance hint, V7 decision 11).
    pub vault_authored: bool,
    /// Vault-relative declared files still matching their recorded hash.
    pub delete: Vec<String>,
    /// Vault-relative declared files modified since install/activation;
    /// always preserved.
    pub keep_modified: Vec<String>,
}

/// Compares the Routine's declared files against the hash lockfile recorded
/// at install/activation (§5.3); for pre-V7 installs without a lockfile, the
/// catalog bytes remain the fallback baseline. Files that differ — edited by
/// the user or their LLM — are preserved, never deleted. Blocking I/O — call
/// from a background thread.
pub fn plan_removal(vault_root: &Path, routine_id: &str) -> Result<RemovalPlan> {
    let catalog_routine = catalog_routine(routine_id)?;
    let manifest = match load_vault_manifest(vault_root, routine_id) {
        Ok(Some(manifest)) => manifest,
        // A broken or missing definition must not block removal: fall back
        // to the legacy or packaged manifest to know what was shipped.
        Ok(None) | Err(_) => match load_legacy_installed_manifest(vault_root, routine_id)? {
            Some(manifest) => manifest,
            None => catalog_routine
                .as_ref()
                .map(|routine| routine.manifest.clone())
                .with_context(|| {
                    format!("Routine {routine_id:?} has no routine.toml and is not in the catalog")
                })?,
        },
    };

    let lock = load_files_lock(vault_root, routine_id)?;
    let mut plan = RemovalPlan {
        routine_name: manifest.name.clone(),
        vault_authored: catalog_routine.is_none(),
        delete: Vec::new(),
        keep_modified: Vec::new(),
    };
    for file in declared_files(&manifest) {
        let path = vault_file_path(vault_root, &file.destination)?;
        let current = match fs::read(&path) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };
        let unmodified = match &lock {
            Some(lock) => lock
                .files
                .get(&file.destination)
                .is_some_and(|recorded| *recorded == content_hash(&current)),
            // Pre-V7 fallback: compare against the catalog asset bytes.
            None => catalog_routine
                .as_ref()
                .and_then(|routine| {
                    file.source
                        .as_deref()
                        .and_then(|source| routine.asset(source))
                })
                .is_some_and(|original| current == original.as_bytes()),
        };
        if unmodified {
            plan.delete.push(file.destination);
        } else {
            plan.keep_modified.push(file.destination);
        }
    }
    // Claude Code bridges are generated, not shipped as assets, so compare
    // each against its freshly generated content instead of a recorded hash.
    for bridge in claude_bridge_files(&manifest) {
        let path = vault_file_path(vault_root, &bridge.destination)?;
        let current = match fs::read_to_string(&path) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };
        if current == bridge.content {
            plan.delete.push(bridge.destination);
        } else {
            plan.keep_modified.push(bridge.destination);
        }
    }
    Ok(plan)
}

#[derive(Debug, PartialEq)]
pub struct RemovalOutcome {
    pub deleted: Vec<String>,
    pub kept_modified: Vec<String>,
}

/// Deletes the Routine's unmodified declared files, its provenance record,
/// and its registry entry. Recomputes the plan at deletion time so edits made
/// while the confirmation dialog was open are still preserved. Blocking I/O —
/// call from a background thread.
pub fn delete_routine(vault_root: &Path, routine_id: &str) -> Result<RemovalOutcome> {
    let plan = plan_removal(vault_root, routine_id)?;
    let mut deleted = Vec::new();
    for destination in &plan.delete {
        let path = vault_file_path(vault_root, destination)?;
        match fs::remove_file(&path) {
            Ok(()) => {
                deleted.push(destination.clone());
                prune_empty_parents(vault_root, &path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("deleting {}", path.display()));
            }
        }
    }

    // The markers are app-owned state, not user files. Left behind, they
    // would instantly "complete" the onboarding of a future reinstall (or
    // re-toast a dead ready marker).
    let marker = onboarding_marker_path(vault_root, routine_id);
    match fs::remove_file(&marker) {
        Ok(()) => prune_empty_parents(vault_root, &marker),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("deleting {}", marker.display()));
        }
    }
    clear_ready_marker(vault_root, routine_id);

    let installed_dir = installed_routine_dir(vault_root, routine_id);
    if installed_dir.exists() {
        fs::remove_dir_all(&installed_dir)
            .with_context(|| format!("deleting {}", installed_dir.display()))?;
        // `installed_dir` is now gone; prune from it as the deleted entry so
        // pruning starts at its parent (`.thock/routines/`). The walk
        // stops at `.thock/` because config.toml keeps it non-empty.
        prune_empty_parents(vault_root, &installed_dir);
    }

    crate::vault::update_routines_registry(vault_root, |installed| {
        installed.retain(|entry| entry.id != routine_id);
    })?;

    Ok(RemovalOutcome {
        deleted,
        kept_modified: plan.keep_modified,
    })
}

/// Removes now-empty directories left behind by a deleted file, walking up
/// toward (but never past) the vault root.
fn prune_empty_parents(vault_root: &Path, deleted_file: &Path) {
    let mut current = deleted_file.parent();
    while let Some(directory) = current {
        if directory == vault_root || !directory.starts_with(vault_root) {
            break;
        }
        // remove_dir refuses to delete non-empty directories, which is
        // exactly the stop condition; any failure just means the directory
        // stays behind.
        if fs::remove_dir(directory).is_err() {
            break;
        }
        current = directory.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{VAULT_CONFIG_FILE, VaultStatus, scaffold_vault};

    fn detect(root: &Path) -> Vault {
        match Vault::detect(root) {
            VaultStatus::Valid(vault) => vault,
            other => panic!("expected valid vault, got {other:?}"),
        }
    }

    #[test]
    fn inbox_catalog_routine_parses() {
        let catalog = catalog().unwrap();
        let routine = catalog
            .iter()
            .find(|routine| routine.manifest.id == INBOX_ROUTINE_ID)
            .expect("the Inbox Routine ships in the catalog");
        let manifest = &routine.manifest;
        assert_eq!(manifest.schema, 2);
        assert_eq!(manifest.icon.as_deref(), Some("envelope"));
        assert_eq!(manifest.doc, "routines/inbox/Inbox.md");
        assert!(manifest.warnings.is_empty(), "{:?}", manifest.warnings);
        // The rail row runs triage (a verb); the log hides in a group; there
        // is deliberately no row that opens inbox/ (V13 §12 #2).
        assert_eq!(manifest.links.len(), 1);
        assert_eq!(manifest.links[0].open, "archives/inbox/triage-log.md");
        assert_eq!(manifest.links[0].group.as_deref(), Some("History"));
        let skills: Vec<(&str, bool)> = manifest
            .skills
            .iter()
            .map(|skill| (skill.id.as_str(), skill.kind.is_setup()))
            .collect();
        assert_eq!(skills, vec![("triage-inbox", false), ("setup-inbox", true)]);
        // The landing zone is scaffolded even though capture doesn't depend
        // on the Routine; the default policy ships as an editable file.
        assert!(
            manifest
                .scaffold
                .iter()
                .any(|entry| matches!(entry, ScaffoldEntry::Dir { path } if path == "inbox"))
        );
        let onboarding = manifest.onboarding.as_ref().unwrap();
        assert!(
            manifest
                .skills
                .iter()
                .any(|skill| skill.file == onboarding.skill)
        );
        for file in declared_files(manifest) {
            if let Some(source) = &file.source {
                assert!(routine.asset(source).is_some(), "missing asset {source:?}");
            }
        }
    }

    #[test]
    fn catalog_parses() {
        let catalog = catalog().unwrap();
        assert_eq!(catalog.len(), 2);
        let manifest = &catalog[0].manifest;
        assert_eq!(manifest.id, TIMELINE_ROUTINE_ID);
        assert_eq!(manifest.schema, 2);
        assert_eq!(manifest.version, 9);
        assert_eq!(manifest.icon.as_deref(), Some("clock"));
        assert_eq!(manifest.doc, "routines/timeline/Timeline.md");
        assert!(manifest.warnings.is_empty(), "{:?}", manifest.warnings);
        assert_eq!(manifest.skills.len(), 5);
        for skill in &manifest.skills {
            assert!(
                skill.file.starts_with("routines/timeline/skills/"),
                "skill outside the Routine's folder: {}",
                skill.file
            );
        }
        // The navigator rows are Routine-owned templated links (V7 §7.4),
        // plus the dashboard surface expressed as a browser link.
        let links: Vec<(&str, &str)> = manifest
            .links
            .iter()
            .map(|link| (link.id.as_str(), link.open.as_str()))
            .collect();
        assert_eq!(
            links,
            vec![
                ("today", "daily/{today}.md"),
                ("yesterday", "daily/{yesterday}.md"),
                ("this-week", "weekly/{this_week}.md"),
                ("last-week", "weekly/{last_week}.md"),
                ("weekly-dashboard", "weekly/site/index.html"),
            ]
        );
        assert!(manifest.links[0].create);
        assert_eq!(manifest.links[4].kind, LinkKind::Browser);
        // Places the rail keeps in reach vs. the ones it demotes (V11 §4).
        let grouped: Vec<(&str, Option<&str>)> = manifest
            .links
            .iter()
            .map(|link| (link.id.as_str(), link.group.as_deref()))
            .collect();
        assert_eq!(
            grouped,
            vec![
                ("today", None),
                ("yesterday", Some("Older notes")),
                ("this-week", None),
                ("last-week", Some("Older notes")),
                ("weekly-dashboard", None),
            ]
        );
        // The one-time steps are setup, not rituals — that classification is
        // what keeps them out of the panel's ritual list.
        let setup: Vec<&str> = manifest
            .skills
            .iter()
            .filter(|skill| skill.kind.is_setup())
            .map(|skill| skill.id.as_str())
            .collect();
        assert_eq!(setup, vec!["connect-google-workspace", "onboarding"]);
        // The onboarding entry points at a file that is also a regular skill,
        // so materialization and removal treat it like any other skill.
        let onboarding = manifest.onboarding.as_ref().unwrap();
        assert!(
            manifest
                .skills
                .iter()
                .any(|skill| skill.file == onboarding.skill)
        );
        // Every declared file with a source must have a bundled asset.
        for file in declared_files(manifest) {
            if let Some(source) = &file.source {
                assert!(
                    catalog[0].asset(source).is_some(),
                    "missing asset {source:?}"
                );
            }
        }
    }

    #[test]
    fn link_groups_and_skill_kinds_parse_and_round_trip() {
        let manifest = parse_manifest(
            r#"
            schema = 2
            id = "finance"
            name = "Finance"
            doc = "routines/finance/Finance.md"
            [[link]]
            name = "Plan"
            open = "finance/plan.md"
            [[link]]
            name = "Last Year"
            open = "finance/2025.md"
            group = "Archive"
            [[skill]]
            id = "sweep"
            name = "Sweep"
            file = "routines/finance/skills/sweep.md"
            [[skill]]
            id = "connect"
            name = "Connect"
            kind = "setup"
            file = "routines/finance/skills/connect.md"
            "#,
        )
        .unwrap();
        assert!(manifest.warnings.is_empty(), "{:?}", manifest.warnings);
        assert_eq!(manifest.links[0].group, None);
        assert_eq!(manifest.links[1].group.as_deref(), Some("Archive"));
        assert_eq!(manifest.skills[0].kind, SkillKind::Ritual);
        assert_eq!(manifest.skills[1].kind, SkillKind::Setup);

        let rendered = render_manifest_toml(&manifest).unwrap();
        // Ritual is the default, so it stays out of a rendered definition.
        assert!(!rendered.contains(r#"kind = "ritual""#));
        assert!(rendered.contains(r#"kind = "setup""#));
        assert_eq!(parse_manifest(&rendered).unwrap(), manifest);
    }

    #[test]
    fn an_unknown_skill_kind_warns_and_stays_a_ritual() {
        let manifest = parse_manifest(
            r#"
            schema = 2
            id = "finance"
            name = "Finance"
            doc = "routines/finance/Finance.md"
            [[skill]]
            id = "sweep"
            name = "Sweep"
            kind = "chore"
            file = "routines/finance/skills/sweep.md"
            "#,
        )
        .unwrap();
        assert_eq!(manifest.skills[0].kind, SkillKind::Ritual);
        assert_eq!(manifest.warnings.len(), 1);
        assert!(
            manifest.warnings[0].contains("chore"),
            "{:?}",
            manifest.warnings
        );
    }

    #[test]
    fn surface_alias_maps_to_browser_link() {
        let manifest = parse_manifest(
            r#"
            schema = 1
            id = "legacy"
            name = "Legacy"
            doc = "areas/Legacy.md"
            [[surface]]
            kind = "dashboard"
            name = "My Dashboard"
            open = "site/index.html"
            "#,
        )
        .unwrap();
        assert_eq!(manifest.links.len(), 1);
        assert_eq!(manifest.links[0].id, "my-dashboard");
        assert_eq!(manifest.links[0].kind, LinkKind::Browser);
        assert_eq!(manifest.links[0].open, "site/index.html");
    }

    #[test]
    fn per_item_icons_parse_and_round_trip() {
        let source = r#"
            schema = 2
            id = "finance"
            name = "Finance"
            doc = "routines/finance/Finance.md"
            [[link]]
            name = "Plan"
            open = "finance/plan.md"
            icon = "hash"
            [[link]]
            name = "Ledger"
            open = "finance/ledger.csv"
            [[skill]]
            id = "close-month"
            name = "Close Month"
            file = "routines/finance/skills/close-month.md"
            icon = "envelope"
            [[skill]]
            id = "reconcile"
            name = "Reconcile"
            file = "routines/finance/skills/reconcile.md"
            "#;
        let manifest = parse_manifest(source).unwrap();
        assert!(manifest.warnings.is_empty(), "{:?}", manifest.warnings);
        assert_eq!(manifest.links[0].icon.as_deref(), Some("hash"));
        assert_eq!(manifest.links[1].icon, None);
        assert_eq!(manifest.skills[0].icon.as_deref(), Some("envelope"));
        assert_eq!(manifest.skills[1].icon, None);

        // Migration rewrites the definition; the icons must survive it.
        let rendered = render_manifest_toml(&manifest).unwrap();
        assert_eq!(parse_manifest(&rendered).unwrap(), manifest);
    }

    #[test]
    fn unknown_keys_warn_instead_of_failing() {
        let manifest = parse_manifest(
            r#"
            schema = 2
            id = "finance"
            name = "Finance"
            doc = "routines/finance/Finance.md"
            colour = "green"
            [[link]]
            name = "Plan"
            open = "finance/plan.md"
            crate = true
            "#,
        )
        .unwrap();
        assert_eq!(manifest.warnings.len(), 2, "{:?}", manifest.warnings);
        assert!(manifest.warnings.iter().any(|w| w.contains("colour")));
        assert!(manifest.warnings.iter().any(|w| w.contains("link.crate")));
        // The typo'd `crate` was ignored; the link still resolved.
        assert!(!manifest.links[0].create);
    }

    #[test]
    fn invalid_manifests_are_errors_not_panics() {
        assert!(parse_manifest("not [valid toml").is_err());
        // Missing required field.
        assert!(parse_manifest("schema = 2\nid = \"x\"\ndoc = \"d.md\"").is_err());
        // Bad id shape.
        assert!(
            parse_manifest("schema = 2\nid = \"../evil\"\nname = \"E\"\ndoc = \"d.md\"").is_err()
        );
    }

    #[test]
    fn scaffold_preinstalls_timeline_routine() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        assert!(dir.path().join("routines/timeline/routine.toml").is_file());
        assert!(dir.path().join("routines/timeline/Timeline.md").is_file());
        assert!(
            dir.path()
                .join("routines/timeline/skills/week-review.md")
                .is_file()
        );
        assert!(dir.path().join("weekly/site/index.html").is_file());
        assert!(dir.path().join("weekly/site/data.js").is_file());
        // Provenance: the hash lockfile under .thock/routines/.
        assert!(files_lock_path(dir.path(), TIMELINE_ROUTINE_ID).is_file());
        // The core authoring files ship with every vault (V7 §7.2).
        assert!(dir.path().join(ROUTINES_REFERENCE_PATH).is_file());
        assert!(dir.path().join(NEW_ROUTINE_SKILL_PATH).is_file());
        // Claude Code bridges are generated for every skill so a `claude`
        // session opened in the vault can invoke them via `/<skill-id>`.
        for skill_id in ["week-review", "wrap-today", "wrap-yesterday", "onboarding"] {
            assert!(
                dir.path()
                    .join(format!(".claude/skills/{skill_id}/SKILL.md"))
                    .is_file(),
                "missing Claude bridge for {skill_id}"
            );
        }

        let vault = detect(dir.path());
        let installed = &vault.config.routines.installed;
        assert_eq!(installed.len(), 2);
        assert_eq!(installed[0].id, TIMELINE_ROUTINE_ID);
        assert_eq!(installed[1].id, INBOX_ROUTINE_ID);
        assert!(installed.iter().all(|entry| entry.enabled));
        assert_eq!(enabled_routine_manifests(&vault).len(), 2);
    }

    #[test]
    fn claude_bridges_carry_frontmatter_and_survive_edits() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let bridge = dir.path().join(".claude/skills/wrap-today/SKILL.md");
        let content = fs::read_to_string(&bridge).unwrap();
        // Diego's choice: explicit `/wrap-today` only, never model-invoked.
        assert!(content.contains("disable-model-invocation: true"));
        // Body points back to the canonical skill file — one source of truth.
        assert!(content.contains("routines/timeline/skills/wrap-today.md"));

        // create-if-missing: a user (or their LLM) edit survives re-materialize.
        fs::write(&bridge, "edited by hand").unwrap();
        let routine = catalog_routine(TIMELINE_ROUTINE_ID).unwrap().unwrap();
        materialize_routine(dir.path(), &routine).unwrap();
        assert_eq!(fs::read_to_string(&bridge).unwrap(), "edited by hand");

        // An edited bridge is preserved on removal, like any user-touched file.
        let plan = plan_removal(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        assert!(
            plan.keep_modified
                .contains(&".claude/skills/wrap-today/SKILL.md".to_string())
        );
    }

    #[test]
    fn reconcile_backfills_missing_files_on_existing_vault() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        // Simulate a vault scaffolded by an older app that lacked bridges and
        // a skill file: drop the whole `.claude` tree and one skill.
        fs::remove_dir_all(dir.path().join(".claude")).unwrap();
        fs::remove_file(
            dir.path()
                .join("routines/timeline/skills/wrap-yesterday.md"),
        )
        .unwrap();
        fs::remove_file(dir.path().join(ROUTINES_REFERENCE_PATH)).unwrap();

        let vault = detect(dir.path());
        reconcile_vault(&vault).unwrap();
        assert!(
            dir.path()
                .join("routines/timeline/skills/wrap-yesterday.md")
                .is_file()
        );
        assert!(
            dir.path()
                .join(".claude/skills/wrap-today/SKILL.md")
                .is_file()
        );
        assert!(dir.path().join(ROUTINES_REFERENCE_PATH).is_file());
    }

    #[test]
    fn reconcile_skips_disabled_routines() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        deactivate_routine(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        deactivate_routine(dir.path(), INBOX_ROUTINE_ID).unwrap();
        fs::remove_dir_all(dir.path().join(".claude")).unwrap();

        let vault = detect(dir.path());
        reconcile_vault(&vault).unwrap();
        assert!(!dir.path().join(".claude").exists());
    }

    #[test]
    fn materialize_never_clobbers() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let skill_path = dir.path().join("routines/timeline/skills/week-review.md");
        fs::write(&skill_path, "my edited skill").unwrap();

        let routine = catalog_routine(TIMELINE_ROUTINE_ID).unwrap().unwrap();
        materialize_routine(dir.path(), &routine).unwrap();
        assert_eq!(fs::read_to_string(&skill_path).unwrap(), "my edited skill");
    }

    #[test]
    fn user_modified_definition_skips_packaged_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let manifest_path = vault_manifest_path(dir.path(), TIMELINE_ROUTINE_ID);
        let edited = fs::read_to_string(&manifest_path)
            .unwrap()
            .replace("Daily & Weekly", "My Rhythm");
        fs::write(&manifest_path, &edited).unwrap();

        let routine = catalog_routine(TIMELINE_ROUTINE_ID).unwrap().unwrap();
        materialize_routine(dir.path(), &routine).unwrap();
        // The user's definition survives (and misses the upgrade, logged).
        assert_eq!(fs::read_to_string(&manifest_path).unwrap(), edited);
        // The panel renders the edited name.
        let vault = detect(dir.path());
        assert_eq!(enabled_routine_manifests(&vault)[0].name, "My Rhythm");
    }

    #[test]
    fn plan_keeps_modified_files_and_delete_preserves_them() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let skill_path = dir.path().join("routines/timeline/skills/week-review.md");
        fs::write(&skill_path, "my edited skill").unwrap();

        let plan = plan_removal(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        assert!(!plan.vault_authored);
        assert_eq!(
            plan.keep_modified,
            vec!["routines/timeline/skills/week-review.md".to_string()]
        );
        assert!(
            plan.delete
                .contains(&"routines/timeline/routine.toml".to_string())
        );
        assert!(
            plan.delete
                .contains(&"routines/timeline/Timeline.md".to_string())
        );
        assert!(plan.delete.contains(&"weekly/site/index.html".to_string()));

        let outcome = delete_routine(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        assert_eq!(
            outcome.kept_modified,
            vec!["routines/timeline/skills/week-review.md".to_string()]
        );
        assert!(skill_path.is_file());
        assert!(!dir.path().join("routines/timeline/Timeline.md").is_file());
        assert!(!dir.path().join("routines/timeline/routine.toml").exists());
        assert!(!dir.path().join("weekly/site").exists());
        // The generated bridges are unmodified, so removal deletes them; the
        // other pre-installed Routine's bridges stay.
        assert!(!dir.path().join(".claude/skills/week-review").exists());
        assert!(
            dir.path()
                .join(".claude/skills/triage-inbox/SKILL.md")
                .is_file()
        );
        assert!(!files_lock_path(dir.path(), TIMELINE_ROUTINE_ID).exists());
        assert!(!installed_routine_dir(dir.path(), TIMELINE_ROUTINE_ID).exists());
        assert!(dir.path().join(VAULT_MARKER_DIR).is_dir());

        let vault = detect(dir.path());
        let installed: Vec<&str> = vault
            .config
            .routines
            .installed
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        assert_eq!(installed, vec![INBOX_ROUTINE_ID]);
        // Core config survives the registry rewrite.
        assert_eq!(vault.config.daily.dir, "daily");
    }

    #[test]
    fn delete_never_touches_user_notes() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let daily_note = dir.path().join("daily/2026-07-20.md");
        fs::write(&daily_note, "my day").unwrap();
        let weekly_note = dir.path().join("weekly/2026-W30.md");
        fs::write(&weekly_note, "my week").unwrap();

        delete_routine(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        assert_eq!(fs::read_to_string(&daily_note).unwrap(), "my day");
        assert_eq!(fs::read_to_string(&weekly_note).unwrap(), "my week");
    }

    #[test]
    fn deactivate_then_reinstall_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();

        deactivate_routine(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        let vault = detect(dir.path());
        assert!(!vault.config.routines.installed[0].enabled);
        assert!(
            !enabled_routine_manifests(&vault)
                .iter()
                .any(|manifest| manifest.id == TIMELINE_ROUTINE_ID)
        );
        // Files stay on disk.
        assert!(dir.path().join("routines/timeline/Timeline.md").is_file());

        install_routine(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        let vault = detect(dir.path());
        assert!(vault.config.routines.installed[0].enabled);
        assert_eq!(vault.config.routines.installed.len(), 2);
    }

    #[test]
    fn install_rematerializes_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let doc_path = dir.path().join("routines/timeline/Timeline.md");
        fs::remove_file(&doc_path).unwrap();

        install_routine(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        assert!(doc_path.is_file());
    }

    fn write_finance_routine(root: &Path) {
        let dir = root.join("routines/finance");
        fs::create_dir_all(dir.join("skills")).unwrap();
        fs::write(
            dir.join("routine.toml"),
            r#"schema = 2
id = "finance"
name = "Finance"
summary = "Money rhythm."
icon = "star"
doc = "routines/finance/Finance.md"
agent_doc = "routines/finance/AGENT.md"

[[link]]
name = "Plan 2026"
open = "finance/plan_2026.md"
kind = "editor"

[[scaffold]]
kind = "dir"
path = "finance"

[[skill]]
id = "friday-finance"
name = "Friday Finance"
file = "routines/finance/skills/friday-finance.md"
summary = "Weekly sweep."
"#,
        )
        .unwrap();
        fs::write(dir.join("Finance.md"), "# Finance\n").unwrap();
        fs::write(dir.join("AGENT.md"), "conventions\n").unwrap();
        fs::write(dir.join("skills/friday-finance.md"), "# Friday Finance\n").unwrap();
    }

    #[test]
    fn discover_activate_and_remove_vault_authored_routine() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        write_finance_routine(dir.path());

        // Discovery: found, valid, and unregistered — never auto-activated.
        let discovered = discover_routines(dir.path());
        let finance = discovered
            .iter()
            .find(|discovered| discovered.id == "finance")
            .unwrap();
        assert_eq!(finance.manifest.as_ref().unwrap().name, "Finance");
        let vault = detect(dir.path());
        assert_eq!(vault.config.routines.installed.len(), 2);

        // Activation: registry, lockfile, bridges, scaffold dirs.
        let manifest = activate_routine(dir.path(), "finance").unwrap();
        assert_eq!(manifest.name, "Finance");
        let vault = detect(dir.path());
        let entry = vault
            .config
            .routines
            .installed
            .iter()
            .find(|entry| entry.id == "finance")
            .unwrap();
        assert!(entry.enabled);
        assert!(files_lock_path(dir.path(), "finance").is_file());
        assert!(dir.path().join("finance").is_dir());
        let bridge = dir.path().join(".claude/skills/friday-finance/SKILL.md");
        assert!(bridge.is_file());
        assert!(
            fs::read_to_string(&bridge)
                .unwrap()
                .contains("routines/finance/skills/friday-finance.md")
        );

        // The panel loads it like any catalog Routine.
        let loaded = enabled_routine_manifests(&vault);
        assert!(loaded.iter().any(|manifest| manifest.id == "finance"));

        // Removal: the lockfile classifies files, unlike V3 where a
        // non-catalog bundle silently deleted nothing (V7 §5.3).
        fs::write(
            dir.path().join("routines/finance/skills/friday-finance.md"),
            "edited ritual",
        )
        .unwrap();
        let plan = plan_removal(dir.path(), "finance").unwrap();
        assert!(plan.vault_authored);
        assert!(
            plan.delete
                .contains(&"routines/finance/routine.toml".to_string())
        );
        assert!(
            plan.delete
                .contains(&"routines/finance/Finance.md".to_string())
        );
        assert!(
            plan.delete
                .contains(&"routines/finance/AGENT.md".to_string())
        );
        assert!(
            plan.keep_modified
                .contains(&"routines/finance/skills/friday-finance.md".to_string())
        );

        let outcome = delete_routine(dir.path(), "finance").unwrap();
        assert!(
            outcome
                .kept_modified
                .contains(&"routines/finance/skills/friday-finance.md".to_string())
        );
        assert!(
            dir.path()
                .join("routines/finance/skills/friday-finance.md")
                .is_file()
        );
        assert!(!dir.path().join("routines/finance/routine.toml").exists());
        let vault = detect(dir.path());
        assert!(
            !vault
                .config
                .routines
                .installed
                .iter()
                .any(|entry| entry.id == "finance")
        );
    }

    #[test]
    fn discovery_surfaces_invalid_manifests() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let broken = dir.path().join("routines/broken");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join("routine.toml"), "schema = 2\nid = \"broken\"\n").unwrap();

        let discovered = discover_routines(dir.path());
        let broken = discovered
            .iter()
            .find(|discovered| discovered.id == "broken")
            .unwrap();
        let error = broken.manifest.as_ref().unwrap_err();
        assert!(error.contains("name"), "unexpected error: {error}");
        // Activation refuses it too.
        assert!(activate_routine(dir.path(), "broken").is_err());
    }

    #[test]
    fn enabled_routine_with_broken_definition_is_an_error_row() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        write_finance_routine(dir.path());
        activate_routine(dir.path(), "finance").unwrap();
        fs::write(
            dir.path().join("routines/finance/routine.toml"),
            "no longer [valid",
        )
        .unwrap();

        let vault = detect(dir.path());
        let loads = enabled_routines(&vault);
        let finance = loads.iter().find(|load| load.id() == "finance").unwrap();
        match finance {
            RoutineLoad::Invalid { error, .. } => {
                assert!(!error.is_empty());
            }
            RoutineLoad::Loaded(_) => panic!("expected an error row"),
        }
        // A mismatched directory/id is invalid too, not silently renamed.
        fs::write(
            dir.path().join("routines/finance/routine.toml"),
            "schema = 2\nid = \"other\"\nname = \"X\"\ndoc = \"routines/finance/Finance.md\"\n",
        )
        .unwrap();
        let vault = detect(dir.path());
        let loads = enabled_routines(&vault);
        assert!(matches!(
            loads.iter().find(|load| load.id() == "finance").unwrap(),
            RoutineLoad::Invalid { .. }
        ));
    }

    #[test]
    fn refresh_fingerprint_tracks_manifests_and_ready_markers() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let base = refresh_fingerprint(dir.path());

        write_finance_routine(dir.path());
        let with_finance = refresh_fingerprint(dir.path());
        assert_ne!(base, with_finance);

        // An edit changes the fingerprint (the panel re-renders, §9 trap 2).
        let manifest_path = vault_manifest_path(dir.path(), "finance");
        let edited = fs::read_to_string(&manifest_path)
            .unwrap()
            .replace("Finance", "Money");
        fs::write(&manifest_path, edited).unwrap();
        let after_edit = refresh_fingerprint(dir.path());
        assert_ne!(with_finance, after_edit);

        // A ready marker changes it too, and activation clears the marker.
        let marker = ready_marker_path(dir.path(), "finance");
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "").unwrap();
        assert_eq!(
            pending_ready_markers(dir.path()),
            vec!["finance".to_string()]
        );
        assert_ne!(after_edit, refresh_fingerprint(dir.path()));
        activate_routine(dir.path(), "finance").unwrap();
        assert!(!marker.exists());
        assert!(pending_ready_markers(dir.path()).is_empty());
    }

    #[test]
    fn resolve_link_templates() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let vault = detect(dir.path());
        let today = chrono::NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();

        let resolved = resolve_link(&vault, "daily/{today}.md", today).unwrap();
        assert_eq!(resolved.relative_path, "daily/2026-07-21.md");
        assert_eq!(resolved.note, Some((NoteKind::Daily, today)));

        let resolved = resolve_link(&vault, "daily/{yesterday}.md", today).unwrap();
        assert_eq!(resolved.relative_path, "daily/2026-07-20.md");

        let resolved = resolve_link(&vault, "weekly/{this_week}.md", today).unwrap();
        assert_eq!(resolved.relative_path, "weekly/2026-W30.md");
        assert_eq!(
            resolved.note,
            Some((
                NoteKind::Weekly,
                chrono::NaiveDate::from_ymd_opt(2026, 7, 20).unwrap()
            ))
        );

        let resolved = resolve_link(&vault, "weekly/{last_week}.md", today).unwrap();
        assert_eq!(resolved.relative_path, "weekly/2026-W29.md");

        // Plain paths pass through with no note association.
        let resolved = resolve_link(&vault, "finance/plan.md", today).unwrap();
        assert_eq!(resolved.relative_path, "finance/plan.md");
        assert_eq!(resolved.note, None);

        // The vocabulary is closed: unknown tokens are errors, not typos
        // that silently open the wrong file.
        assert!(resolve_link(&vault, "finance/{month}.md", today).is_err());
        assert!(resolve_link(&vault, "daily/{today.md", today).is_err());
    }

    #[test]
    fn ensure_link_target_creates_notes_from_template() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();
        let vault = detect(dir.path());
        let today = chrono::NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
        let time = chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap();

        // A date-template link creates from the note template, like Today.
        let resolved = resolve_link(&vault, "daily/{today}.md", today).unwrap();
        let path = ensure_link_target(&vault, true, &resolved, time).unwrap();
        assert_eq!(path, dir.path().join("daily/2026-07-20.md"));
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("# Monday, July 20, 2026")
        );

        // Existing files are never touched.
        fs::write(&path, "user edits").unwrap();
        ensure_link_target(&vault, true, &resolved, time).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "user edits");

        // create = false leaves missing targets missing.
        let resolved = resolve_link(&vault, "finance/plan.md", today).unwrap();
        let path = ensure_link_target(&vault, false, &resolved, time).unwrap();
        assert!(!path.exists());

        // A plain create link becomes an empty file.
        let path = ensure_link_target(&vault, true, &resolved, time).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
    }

    /// The §2 migration: a pre-V7 vault (old registry key, `.thock/
    /// areas/` provenance, `areas/` + `skills/timeline/` layout) opens and
    /// migrates conservatively.
    #[test]
    fn migrates_pre_v7_vault_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Core scaffold, minus the Routine: mimic V6 by hand.
        fs::create_dir_all(root.join(VAULT_MARKER_DIR)).unwrap();
        fs::write(
            root.join(VAULT_MARKER_DIR).join(VAULT_CONFIG_FILE),
            "schema = 1\n\n[[areas.installed]]\nid = \"timeline\"\nenabled = true\nversion = 2\n",
        )
        .unwrap();
        let legacy_manifest = r#"schema  = 1
id      = "timeline"
name    = "Daily & Weekly"
version = 2
doc     = "areas/Timeline.md"

[[scaffold]]
kind = "dir"
path = "weekly/site"

[[scaffold]]
kind   = "file"
path   = "weekly/site/index.html"
source = "assets/index.html"

[[scaffold]]
kind   = "file"
path   = "weekly/site/data.js"
source = "assets/data.seed.js"

[[skill]]
id      = "week-review"
name    = "Week Review"
file    = "skills/timeline/week-review.md"

[[skill]]
id      = "wrap-today"
name    = "Wrap Today"
file    = "skills/timeline/wrap-today.md"

[[surface]]
kind = "dashboard"
name = "Weekly Dashboard"
open = "weekly/site/index.html"
"#;
        let legacy_dir = root.join(VAULT_MARKER_DIR).join("areas").join("timeline");
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("manifest.toml"), legacy_manifest).unwrap();
        // Old-layout files: the doc and wrap-today ship pristine (current
        // package bytes); week-review carries a user edit.
        fs::create_dir_all(root.join("areas")).unwrap();
        fs::create_dir_all(root.join("skills/timeline")).unwrap();
        fs::create_dir_all(root.join("weekly/site")).unwrap();
        fs::write(root.join("areas/Timeline.md"), TIMELINE_DOC).unwrap();
        fs::write(
            root.join("skills/timeline/wrap-today.md"),
            TIMELINE_WRAP_TODAY_SKILL,
        )
        .unwrap();
        fs::write(root.join("skills/timeline/week-review.md"), "my edits").unwrap();
        fs::write(root.join("weekly/site/index.html"), TIMELINE_DASHBOARD_HTML).unwrap();
        fs::write(root.join("weekly/site/data.js"), "existing feed").unwrap();

        let vault = detect(root);
        assert_eq!(vault.config.routines.installed.len(), 1);
        reconcile_vault(&vault).unwrap();

        // App-owned provenance moved silently; the legacy manifest is gone.
        assert!(!root.join(VAULT_MARKER_DIR).join("areas").exists());
        assert!(files_lock_path(root, TIMELINE_ROUTINE_ID).is_file());
        assert!(!legacy_installed_manifest_path(root, TIMELINE_ROUTINE_ID).exists());

        // Unmodified shipped files moved to the new layout; the old dirs
        // were pruned as they emptied.
        assert_eq!(
            fs::read_to_string(root.join("routines/timeline/Timeline.md")).unwrap(),
            TIMELINE_DOC
        );
        assert!(!root.join("areas").exists());
        assert_eq!(
            fs::read_to_string(root.join("routines/timeline/skills/wrap-today.md")).unwrap(),
            TIMELINE_WRAP_TODAY_SKILL
        );

        // The modified skill stayed put, and the written definition points
        // at it (never-clobber).
        assert_eq!(
            fs::read_to_string(root.join("skills/timeline/week-review.md")).unwrap(),
            "my edits"
        );
        let manifest = load_vault_manifest(root, TIMELINE_ROUTINE_ID)
            .unwrap()
            .unwrap();
        let week_review = manifest
            .skills
            .iter()
            .find(|skill| skill.id == "week-review")
            .unwrap();
        assert_eq!(week_review.file, "skills/timeline/week-review.md");
        let wrap_today = manifest
            .skills
            .iter()
            .find(|skill| skill.id == "wrap-today")
            .unwrap();
        assert_eq!(wrap_today.file, "routines/timeline/skills/wrap-today.md");

        // User data at unchanged paths was not touched.
        assert_eq!(
            fs::read_to_string(root.join("weekly/site/data.js")).unwrap(),
            "existing feed"
        );

        // The registry moved to the new key, stamped with the migrated
        // definition's version.
        let raw = fs::read_to_string(root.join(VAULT_MARKER_DIR).join(VAULT_CONFIG_FILE)).unwrap();
        assert!(raw.contains("[[routines.installed]]"), "{raw}");
        assert!(!raw.contains("[[areas.installed]]"), "{raw}");
        let vault = detect(root);
        assert_eq!(vault.config.routines.installed[0].version, 9);

        // Idempotent: a second pass changes nothing.
        let vault = detect(root);
        reconcile_vault(&vault).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("skills/timeline/week-review.md")).unwrap(),
            "my edits"
        );
        let manifest_after = load_vault_manifest(root, TIMELINE_ROUTINE_ID)
            .unwrap()
            .unwrap();
        assert_eq!(manifest_after, manifest);
    }

    #[test]
    fn onboarding_state_machine() {
        use OnboardingCheck::*;
        use OnboardingState::*;
        let now = Utc::now();
        let fresh = Some(now - chrono::Duration::hours(1));
        let stale = Some(now - chrono::Duration::hours(25));

        // Pending: marker wins; then the 24 h window decides.
        assert_eq!(
            check_onboarding(Some(Pending), fresh, true, now),
            MarkOnboarded
        );
        assert_eq!(
            check_onboarding(Some(Pending), stale, true, now),
            MarkOnboarded
        );
        assert_eq!(check_onboarding(Some(Pending), fresh, false, now), Nothing);
        assert_eq!(
            check_onboarding(Some(Pending), stale, false, now),
            PromptExpiry
        );
        // Pending with no timestamp is malformed — expire it rather than
        // badge forever.
        assert_eq!(
            check_onboarding(Some(Pending), None, false, now),
            PromptExpiry
        );

        // Expired: a late marker still completes; otherwise permanent silence.
        assert_eq!(
            check_onboarding(Some(Expired), stale, true, now),
            MarkOnboarded
        );
        assert_eq!(check_onboarding(Some(Expired), stale, false, now), Nothing);

        // Onboarded: terminal.
        assert_eq!(check_onboarding(Some(Onboarded), stale, true, now), Nothing);
        assert_eq!(
            check_onboarding(Some(Onboarded), stale, false, now),
            Nothing
        );

        // Pre-V5 / scaffolded (no state): quiet, but a marker completes.
        assert_eq!(check_onboarding(None, None, false, now), Nothing);
        assert_eq!(check_onboarding(None, None, true, now), MarkOnboarded);
    }

    #[test]
    fn install_sets_pending_only_for_new_entries() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();

        // Scaffolded pre-install: no onboarding state (quiet, decision 14).
        let vault = detect(dir.path());
        assert_eq!(vault.config.routines.installed[0].onboarding_state, None);

        // Re-enabling and reinstalling files never starts onboarding.
        deactivate_routine(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        install_routine(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        let vault = detect(dir.path());
        assert_eq!(vault.config.routines.installed[0].onboarding_state, None);
        assert_eq!(
            vault.config.routines.installed[0].onboarding_installed_at,
            None
        );

        // A fresh entry after full removal enters the flow as pending — and a
        // stale done marker from the previous install must not instantly
        // "complete" it.
        let marker = onboarding_marker_path(dir.path(), TIMELINE_ROUTINE_ID);
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "done last time").unwrap();
        delete_routine(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        assert!(!marker.exists(), "removal must clean the done marker");
        install_routine(dir.path(), TIMELINE_ROUTINE_ID).unwrap();
        let vault = detect(dir.path());
        let entry = vault
            .config
            .routines
            .installed
            .iter()
            .find(|entry| entry.id == TIMELINE_ROUTINE_ID)
            .unwrap();
        assert_eq!(entry.onboarding_state, Some(OnboardingState::Pending));
        assert!(entry.onboarding_installed_at.is_some());
    }

    #[test]
    fn set_onboarding_state_reports_change() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_vault(dir.path()).unwrap();

        assert!(
            set_onboarding_state(dir.path(), TIMELINE_ROUTINE_ID, OnboardingState::Onboarded)
                .unwrap()
        );
        // Idempotent: the second transition reports no change, so one-shot
        // effects (the tour) can key off the return value.
        assert!(
            !set_onboarding_state(dir.path(), TIMELINE_ROUTINE_ID, OnboardingState::Onboarded)
                .unwrap()
        );
        let vault = detect(dir.path());
        assert_eq!(
            vault.config.routines.installed[0].onboarding_state,
            Some(OnboardingState::Onboarded)
        );
        // Unknown routines are a no-op, not an error.
        assert!(!set_onboarding_state(dir.path(), "nope", OnboardingState::Expired).unwrap());
    }

    /// The V5 spec §8 integration loop, minus the GPUI/terminal layer: a fake
    /// agent script receives the kickoff exactly as the PTY spawn would (one
    /// argv element, no shell interpolation of the command line), runs in the
    /// vault root, writes the done marker — and the state machine completes.
    #[cfg(unix)]
    #[test]
    // Blocking on a child process is the point of this test; there is no
    // async executor here.
    #[allow(clippy::disallowed_methods)]
    fn fake_agent_drives_launch_to_marker_loop() {
        use std::os::unix::fs::PermissionsExt as _;

        let vault = tempfile::tempdir().unwrap();
        scaffold_vault(vault.path()).unwrap();
        // Only a fresh registry entry goes pending, so simulate a real
        // Add-Routine: full removal, then install.
        delete_routine(vault.path(), TIMELINE_ROUTINE_ID).unwrap();
        install_routine(vault.path(), TIMELINE_ROUTINE_ID).unwrap();

        let scripts = tempfile::tempdir().unwrap();
        let script_path = scripts.path().join("fake-agent");
        fs::write(
            &script_path,
            "#!/bin/sh\n\
             printf '%s' \"$1\" > kickoff.txt\n\
             mkdir -p .thock/state/onboarded\n\
             printf 'migrated 3 notes' > .thock/state/onboarded/timeline\n",
        )
        .unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        // The connect flow stores a command line; quoting survives shlex.
        let command_line = shlex::try_quote(script_path.to_str().unwrap())
            .unwrap()
            .into_owned();
        let kickoff = crate::agent::run_skill_kickoff("routines/timeline/skills/onboarding.md");
        let launch = crate::agent::build_launch(&command_line, Some(&kickoff)).unwrap();
        let status = std::process::Command::new(&launch.program)
            .args(&launch.args)
            .current_dir(vault.path())
            .status()
            .unwrap();
        assert!(status.success());

        // The agent saw the kickoff as one argument and left the marker.
        assert_eq!(
            fs::read_to_string(vault.path().join("kickoff.txt")).unwrap(),
            "Read and execute routines/timeline/skills/onboarding.md"
        );
        let marker = onboarding_marker_path(vault.path(), TIMELINE_ROUTINE_ID);
        assert!(marker.is_file());

        // The watcher's decision path completes the flow exactly once.
        let entry = detect(vault.path()).config.routines.installed[0].clone();
        assert_eq!(
            check_onboarding(
                entry.onboarding_state,
                entry.onboarding_installed_at,
                marker.is_file(),
                Utc::now(),
            ),
            OnboardingCheck::MarkOnboarded
        );
        assert!(
            set_onboarding_state(
                vault.path(),
                TIMELINE_ROUTINE_ID,
                OnboardingState::Onboarded
            )
            .unwrap()
        );
        assert!(
            !set_onboarding_state(
                vault.path(),
                TIMELINE_ROUTINE_ID,
                OnboardingState::Onboarded
            )
            .unwrap()
        );
    }

    #[test]
    fn onboarding_marker_lives_under_state_dir() {
        let root = Path::new("/vault");
        assert_eq!(
            onboarding_marker_path(root, "timeline"),
            root.join(".thock/state/onboarded/timeline")
        );
        assert_eq!(
            ready_marker_path(root, "finance"),
            root.join(".thock/state/routine-ready/finance")
        );
    }

    #[test]
    fn vault_file_path_rejects_escapes() {
        let root = Path::new("/vault");
        assert!(vault_file_path(root, "notes/ok.md").is_ok());
        assert!(vault_file_path(root, "../outside.md").is_err());
        assert!(vault_file_path(root, "/etc/passwd").is_err());
        assert!(vault_file_path(root, "").is_err());
    }
}
