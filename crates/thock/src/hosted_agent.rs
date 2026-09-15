//! The hosted Thock Agent's harness (V25 §5): Pi, launched over ACP through
//! the `pi-acp` adapter, both installed with the managed Node runtime into a
//! Thock-owned folder. The per-user gateway key, the model behind the
//! requested tier, and the note-taking system prompt reach Pi through a
//! private Pi config directory and the process environment, so nothing is
//! read from or written to the user's own `~/.pi`.

use agent_servers::AcpConnection;
use anyhow::{Context as _, Result};
use chrono::{Datelike as _, NaiveDate};
use collections::HashMap;
use fs::{Fs, RemoveOptions};
use gpui::{AppContext as _, AsyncApp, Entity};
use node_runtime::{NodeRuntime, VersionStrategy};
use project::Project;
use project::agent_server_store::{AgentId, AgentServerCommand};
use semver::Version;
use std::fmt::Write as _;
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use crate::agent::ModelTier;
use crate::notes::NoteKind;
use crate::routines::RoutineManifest;
use crate::vault::Vault;

/// The ACP adapter and the harness it drives, pinned so a registry or npm
/// change never reaches users untested (spec §6: `pi-acp` is a one-person
/// MVP and Thock must be ready to fork it).
pub const PI_ACP_PACKAGE: &str = "pi-acp";
pub const PI_ACP_VERSION: &str = "0.0.33";
pub const PI_PACKAGE: &str = "@earendil-works/pi-coding-agent";
pub const PI_VERSION: &str = "0.85.1";
pub const AGENT_ID: &str = "thock-hosted-agent";

/// A note-taking prompt in place of Pi's coding one: who the agent is, how it
/// speaks, and the rules that must hold even in a vault whose `AGENTS.md` was
/// edited away (spec `v27-agent-session-prompt.md` §5.2).
pub const SYSTEM_PROMPT: &str = include_str!("../assets/hosted-agent/SYSTEM.md");
/// The Pi extension that adds the `append` tool and refuses whole-file
/// writes over a note with content, so "append, don't rewrite" holds even
/// when the model forgets the prompt.
pub const VAULT_GUARD_EXTENSION: &str = include_str!("../assets/hosted-agent/vault-guard.ts");

/// Where the npm packages land, beside Zed's own registry-installed agents.
pub fn install_dir() -> PathBuf {
    paths::external_agents_dir().join("thock-hosted")
}

/// Pi's config directory for the hosted path, one per tier *and* vault: two
/// sessions on different tiers never race over one `settings.json`, and two
/// open vaults never race over one another's context block (spec §5.4).
pub fn pi_config_dir(tier: ModelTier, vault_root: Option<&Path>) -> PathBuf {
    let segment = match vault_root {
        Some(root) => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            root.hash(&mut hasher);
            format!("{}-{:016x}", tier.as_str(), hasher.finish())
        }
        None => tier.as_str().to_string(),
    };
    paths::data_dir()
        .join("thock")
        .join("hosted-agent")
        .join(segment)
}

pub struct InstalledHarness {
    pub node_binary: PathBuf,
    pub pi_acp_entry: PathBuf,
    /// The `pi` shim npm links into `node_modules/.bin`.
    pub pi_command: PathBuf,
    pub bin_dir: PathBuf,
}

/// Installs (or updates to the pinned versions) the adapter and Pi with the
/// managed Node runtime. Idempotent and cheap when nothing changed.
pub async fn ensure_installed(node: &NodeRuntime, fs: &Arc<dyn Fs>) -> Result<InstalledHarness> {
    let dir = install_dir();
    fs.create_dir(&dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;
    let node_binary = node
        .binary_path()
        .await
        .context("locating the bundled Node runtime")?;
    let node_modules = dir.join("node_modules");
    let pi_acp_entry = node_modules
        .join(PI_ACP_PACKAGE)
        .join("dist")
        .join("index.js");
    let pi_entry = node_modules
        .join(PI_PACKAGE)
        .join("dist")
        .join("bundle")
        .join("cli.js");
    let pi_acp_version = Version::parse(PI_ACP_VERSION)?;
    let pi_version = Version::parse(PI_VERSION)?;
    let needs_pi_acp = node
        .should_install_npm_package(
            PI_ACP_PACKAGE,
            &pi_acp_entry,
            &dir,
            VersionStrategy::Pin(&pi_acp_version),
        )
        .await;
    let needs_pi = node
        .should_install_npm_package(
            PI_PACKAGE,
            &pi_entry,
            &dir,
            VersionStrategy::Pin(&pi_version),
        )
        .await;
    if needs_pi_acp || needs_pi {
        // Not `npm_install_packages`: its 5-second fetch timeout is sized for
        // small language servers, and Pi's dependency tree has hundreds of
        // tarballs, so one slow download fails the whole install on an
        // ordinary connection.
        let pi_acp = format!("{PI_ACP_PACKAGE}@{PI_ACP_VERSION}");
        let pi = format!("{PI_PACKAGE}@{PI_VERSION}");
        node.run_npm_subcommand(
            Some(&dir),
            "install",
            &[
                pi_acp.as_str(),
                pi.as_str(),
                "--save-exact",
                "--fetch-retries",
                "5",
                "--fetch-retry-mintimeout",
                "2000",
                "--fetch-retry-maxtimeout",
                "30000",
                "--fetch-timeout",
                "120000",
            ],
        )
        .await
        .context("installing the Thock Agent")?;
    }
    let bin_dir = node_modules.join(".bin");
    let pi_command = if cfg!(windows) {
        bin_dir.join("pi.cmd")
    } else {
        bin_dir.join("pi")
    };
    Ok(InstalledHarness {
        node_binary,
        pi_acp_entry,
        pi_command,
        bin_dir,
    })
}

/// Pi's `settings.json` for a tier: the OpenRouter provider, the model the
/// plan maps the tier to, and every startup nicety switched off so the
/// process is quiet, trusts nothing project-local, and phones no one.
pub fn pi_settings(model_id: &str) -> String {
    let settings = serde_json::json!({
        "defaultProvider": "openrouter",
        "defaultModel": model_id,
        "quietStartup": true,
        "defaultProjectTrust": "never",
        "enableInstallTelemetry": false,
        "enableSkillCommands": false,
    });
    serde_json::to_string_pretty(&settings).unwrap_or_default()
}

fn vault_relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// The live facts a session needs and no file in the vault carries: today's
/// date, where this week's notes live, the language the vault was set to, and
/// which Routines are installed (spec `v27-agent-session-prompt.md` §5.3).
///
/// Pi loads this straight after `SYSTEM.md` and before the vault's own
/// `AGENTS.md`, so the user's file still has the last word.
pub fn compose_vault_context(
    vault: &Vault,
    routines: &[RoutineManifest],
    has_profile: bool,
    memory_index: Option<&str>,
    today: NaiveDate,
) -> String {
    let week = today.iso_week();
    let mut context = String::from("# Right now\n\n");
    let _ = writeln!(
        context,
        "Today is {} ({today}) — week {}-W{:02}.",
        today.format("%A, %-d %B %Y"),
        week.year(),
        week.week(),
    );
    let _ = writeln!(
        context,
        "This person's vault is the folder {}. Everything you do happens inside it.\n",
        vault.root.display(),
    );

    let daily = vault_relative(&vault.note_path(NoteKind::Daily, today), &vault.root);
    let _ = writeln!(
        context,
        "- Today's note: `{daily}` (make it from `{}` when it isn't there yet).",
        vault.config.daily.template,
    );
    if let Some((_, monday)) = crate::notes::TimelineEntry::ThisWeek.resolve(today) {
        let weekly = vault_relative(&vault.note_path(NoteKind::Weekly, monday), &vault.root);
        let _ = writeln!(
            context,
            "- This week's note: `{weekly}` (from `{}`).",
            vault.config.weekly.template,
        );
    }
    let headings = &vault.config.backlog.headings;
    let _ = writeln!(
        context,
        "- Tasks: `{}`, under the headings `{}`, `{}` and `{}`.",
        vault.config.backlog.file, headings.soon, headings.someday, headings.completed,
    );
    let planner_heading = vault.config.day_planner.heading.trim();
    if !planner_heading.is_empty() {
        let _ = writeln!(
            context,
            "- The day's plan lives under the `{planner_heading}` heading of the daily note.",
        );
    }
    if has_profile {
        context.push_str(
            "- `profile.md` says who this person is and what you may look at — read it \
             before you start.\n",
        );
    }

    context.push_str("\n## Language\n\n");
    match language_label(vault) {
        Some(label) => {
            let _ = writeln!(
                context,
                "This vault is set to {label}. Speak and write in it, from your first \
                 greeting onwards.",
            );
        }
        None => context.push_str(
            "No language has been set for this vault, so answer in whatever language the \
             person writes to you in.\n",
        ),
    }

    context.push_str("\n## Routines\n\n");
    if routines.is_empty() {
        context.push_str(
            "None are installed yet. `skills/thock/new-routine.md` is the ritual that builds \
             one, and the Routines panel is where they are added.\n",
        );
    } else {
        for routine in routines {
            let _ = writeln!(
                context,
                "- **{}** — `routines/{}/`, explained in `{}`.",
                routine.name, routine.id, routine.doc,
            );
            if !routine.skills.is_empty() {
                let rituals = routine
                    .skills
                    .iter()
                    .map(|skill| format!("{} (`{}`)", skill.name, skill.file))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(context, "  Rituals: {rituals}.");
            }
        }
    }

    context.push_str("\n## What you already know\n\n");
    match memory_index {
        Some(index) => {
            context.push_str(
                "This is `memory/index.md`, what past sessions learned about this person. \
                 Treat it as things you already know. Where a line points at a page under \
                 `memory/`, open that page when the conversation touches it. When they tell \
                 you something that will still be true next month, or correct you, add one \
                 dated line to `memory/inbox.md`; the Reflect ritual files it. Never edit \
                 `memory/index.md` or the other memory pages outside that ritual.\n\n",
            );
            context.push_str(index);
            context.push('\n');
        }
        None => context.push_str(
            "Nothing yet: no session has learned anything about this person, or the \
             `memory/` folder is missing. When they tell you something that will still be \
             true next month, add one dated line to `memory/inbox.md` (create it if needed); \
             the Reflect ritual files it.\n",
        ),
    }
    context
}

/// How the vault's language should be named to the agent: the user's own
/// words when the Set Language ritual recorded them, the BCP 47 tag when it
/// only recorded that.
fn language_label(vault: &Vault) -> Option<String> {
    let language = vault.config.language.as_ref()?;
    let name = language
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let tag = language
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|tag| !tag.is_empty());
    match (name, tag) {
        (Some(name), Some(tag)) => Some(format!("**{name}** (`{tag}`)")),
        (Some(name), None) => Some(format!("**{name}**")),
        (None, Some(tag)) => Some(format!("`{tag}`")),
        (None, None) => None,
    }
}

/// `compose_vault_context` with the blocking vault reads around it. Returns
/// `None` when the workspace is not a vault, which clears any block a previous
/// session left behind. Blocking I/O — call from a background thread.
pub fn gather_vault_context(vault: Option<&Vault>, today: NaiveDate) -> Option<String> {
    let vault = vault?;
    let routines = crate::routines::enabled_routine_manifests(vault);
    let has_profile = vault.root.join("profile.md").is_file();
    let memory_index =
        crate::memory::read_index_capped(&vault.root, vault.config.memory.index_lines);
    Some(compose_vault_context(
        vault,
        &routines,
        has_profile,
        memory_index.as_deref(),
        today,
    ))
}

/// Writes the session's Pi config directory and returns it.
pub async fn write_pi_config(
    fs: &Arc<dyn Fs>,
    tier: ModelTier,
    model_id: &str,
    vault_root: Option<&Path>,
    vault_context: Option<&str>,
) -> Result<PathBuf> {
    let dir = pi_config_dir(tier, vault_root);
    fs.create_dir(&dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;
    fs.atomic_write(dir.join("settings.json"), pi_settings(model_id))
        .await
        .context("writing the Thock Agent settings")?;
    fs.atomic_write(dir.join("SYSTEM.md"), SYSTEM_PROMPT.to_string())
        .await
        .context("writing the Thock Agent prompt")?;
    let extensions = dir.join("extensions");
    fs.create_dir(&extensions)
        .await
        .with_context(|| format!("creating {}", extensions.display()))?;
    fs.atomic_write(
        extensions.join("vault-guard.ts"),
        VAULT_GUARD_EXTENSION.to_string(),
    )
    .await
    .context("writing the Thock Agent vault guard")?;
    let context_path = dir.join("APPEND_SYSTEM.md");
    match vault_context {
        Some(context) => fs
            .atomic_write(context_path, context.to_string())
            .await
            .context("writing the Thock Agent vault context")?,
        None => fs
            .remove_file(
                &context_path,
                RemoveOptions {
                    recursive: false,
                    ignore_if_not_exists: true,
                },
            )
            .await
            .context("clearing the Thock Agent vault context")?,
    }
    Ok(dir)
}

/// The adapter's launch command. `base_env` is the user's shell environment
/// (so Pi finds git and friends); the gateway key, Pi's private config
/// directory, and a PATH that resolves the managed Node and the `pi` shim
/// are layered on top.
pub fn launch_command(
    harness: &InstalledHarness,
    pi_config_dir: &Path,
    api_key: &str,
    mut base_env: HashMap<String, String>,
) -> Result<AgentServerCommand> {
    let inherited_path = base_env
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let node_dir = harness
        .node_binary
        .parent()
        .map(Path::to_path_buf)
        .context("the Node binary has no parent directory")?;
    let path = std::env::join_paths(
        [harness.bin_dir.clone(), node_dir]
            .into_iter()
            .chain(std::env::split_paths(&inherited_path)),
    )
    .context("building the agent PATH")?;
    base_env.insert("PATH".to_string(), path.to_string_lossy().into_owned());
    base_env.insert("OPENROUTER_API_KEY".to_string(), api_key.to_string());
    base_env.insert(
        "PI_CODING_AGENT_DIR".to_string(),
        pi_config_dir.to_string_lossy().into_owned(),
    );
    base_env.insert(
        "PI_ACP_PI_COMMAND".to_string(),
        harness.pi_command.to_string_lossy().into_owned(),
    );
    base_env.insert("PI_SKIP_VERSION_CHECK".to_string(), "1".to_string());
    Ok(AgentServerCommand {
        path: harness.node_binary.clone(),
        args: vec![harness.pi_acp_entry.to_string_lossy().into_owned()],
        env: Some(base_env),
    })
}

/// Everything before the process starts: install, config, environment.
pub async fn prepare_launch(
    project: &Entity<Project>,
    vault: Option<Vault>,
    tier: ModelTier,
    model_id: &str,
    api_key: &str,
    cx: &mut AsyncApp,
) -> Result<AgentServerCommand> {
    let (node, fs, environment) = project.read_with(cx, |project, cx| {
        (
            project.agent_server_store().read(cx).node_runtime(),
            project.fs().clone(),
            project.environment().clone(),
        )
    });
    let node = node.context("the Thock Agent needs a local vault; this workspace is remote")?;
    let harness = ensure_installed(&node, &fs).await?;
    let vault_root = vault.as_ref().map(|vault| vault.root.clone());
    let vault_context = cx
        .background_spawn(async move {
            gather_vault_context(vault.as_ref(), chrono::Local::now().date_naive())
        })
        .await;
    let pi_config_dir = write_pi_config(
        &fs,
        tier,
        model_id,
        vault_root.as_deref(),
        vault_context.as_deref(),
    )
    .await?;
    let mut env: HashMap<String, String> = environment
        .update(cx, |environment, cx| environment.default_environment(cx))
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();
    env.extend(cx.update(agent_servers::load_proxy_env));
    launch_command(&harness, &pi_config_dir, api_key, env)
}

/// Spawns the adapter and completes the ACP handshake. Dropping the returned
/// connection (with every thread on it) ends the process.
pub async fn connect(
    project: Entity<Project>,
    command: AgentServerCommand,
    cx: &mut AsyncApp,
) -> Result<Rc<dyn acp_thread::AgentConnection>> {
    let store = project.read_with(cx, |project, _| project.agent_server_store().downgrade());
    let connection = AcpConnection::stdio(
        AgentId::new(AGENT_ID),
        project,
        command,
        store,
        None,
        Default::default(),
        cx,
    )
    .await
    .context("starting the Thock Agent")?;
    Ok(Rc::new(connection))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness() -> InstalledHarness {
        InstalledHarness {
            node_binary: PathBuf::from("/managed/node/bin/node"),
            pi_acp_entry: PathBuf::from("/agents/thock-hosted/node_modules/pi-acp/dist/index.js"),
            pi_command: PathBuf::from("/agents/thock-hosted/node_modules/.bin/pi"),
            bin_dir: PathBuf::from("/agents/thock-hosted/node_modules/.bin"),
        }
    }

    #[test]
    fn launch_command_runs_the_adapter_under_managed_node_with_the_key_in_env() {
        let mut base = HashMap::default();
        base.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
        base.insert("HOME".to_string(), "/Users/me".to_string());
        let command = launch_command(
            &harness(),
            Path::new("/data/pi/default"),
            "sk-or-v1-x",
            base,
        )
        .unwrap();
        assert_eq!(command.path, PathBuf::from("/managed/node/bin/node"));
        assert_eq!(
            command.args,
            vec!["/agents/thock-hosted/node_modules/pi-acp/dist/index.js".to_string()]
        );
        let env = command.env.unwrap();
        assert_eq!(env["OPENROUTER_API_KEY"], "sk-or-v1-x");
        assert_eq!(env["PI_CODING_AGENT_DIR"], "/data/pi/default");
        assert_eq!(
            env["PI_ACP_PI_COMMAND"],
            "/agents/thock-hosted/node_modules/.bin/pi"
        );
        assert_eq!(env["PI_SKIP_VERSION_CHECK"], "1");
        assert_eq!(env["HOME"], "/Users/me", "the shell environment is kept");
        let path = std::env::split_paths(&env["PATH"]).collect::<Vec<_>>();
        assert_eq!(
            path,
            vec![
                PathBuf::from("/agents/thock-hosted/node_modules/.bin"),
                PathBuf::from("/managed/node/bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
            ],
            "the shim and the managed node must come first so `#!/usr/bin/env node` resolves"
        );
    }

    #[test]
    fn pi_settings_pin_the_provider_and_model_and_stay_quiet() {
        let settings: serde_json::Value =
            serde_json::from_str(&pi_settings("google/gemini-2.5-flash")).unwrap();
        assert_eq!(settings["defaultProvider"], "openrouter");
        assert_eq!(settings["defaultModel"], "google/gemini-2.5-flash");
        assert_eq!(settings["quietStartup"], true);
        assert_eq!(settings["defaultProjectTrust"], "never");
        assert_eq!(settings["enableInstallTelemetry"], false);
    }

    #[test]
    fn config_dirs_are_per_tier_and_per_vault() {
        let vault = Path::new("/Users/me/Thock");
        assert_ne!(
            pi_config_dir(ModelTier::Default, Some(vault)),
            pi_config_dir(ModelTier::Fast, Some(vault)),
        );
        assert_ne!(
            pi_config_dir(ModelTier::Default, Some(vault)),
            pi_config_dir(ModelTier::Default, Some(Path::new("/Users/me/Other"))),
        );
        assert_eq!(
            pi_config_dir(ModelTier::Default, Some(vault)),
            pi_config_dir(ModelTier::Default, Some(vault)),
            "the same vault must resolve to the same directory across launches"
        );
        assert!(!SYSTEM_PROMPT.trim().is_empty());
    }

    #[test]
    fn the_prompt_and_the_guard_agree_on_the_tools() {
        for tool in ["`append`", "`edit`", "`write`", "`read`"] {
            assert!(
                SYSTEM_PROMPT.contains(tool),
                "the prompt must explain {tool}"
            );
        }
        assert!(VAULT_GUARD_EXTENSION.contains("name: \"append\""));
        assert!(VAULT_GUARD_EXTENSION.contains("pi.on(\"tool_call\""));
        assert!(VAULT_GUARD_EXTENSION.contains("export default function"));
    }

    fn vault_at(root: &str) -> Vault {
        Vault {
            root: PathBuf::from(root),
            config: crate::vault::VaultConfig::default(),
        }
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 14).unwrap()
    }

    #[test]
    fn context_states_the_date_the_week_and_this_week_notes() {
        let context =
            compose_vault_context(&vault_at("/Users/me/Thock"), &[], false, None, today());
        assert!(context.contains("Monday, 14 September 2026 (2026-09-14)"));
        assert!(context.contains("week 2026-W38"));
        assert!(context.contains("/Users/me/Thock"));
        assert!(context.contains("`daily/2026-09-14.md`"));
        assert!(context.contains("`weekly/2026-W38.md`"));
        assert!(context.contains("`templates/daily.md`"));
        assert!(
            !context.contains("profile.md"),
            "a vault without a profile must not claim one"
        );
    }

    #[test]
    fn context_names_the_configured_headings_not_the_english_defaults() {
        let mut vault = vault_at("/Users/me/Cofre");
        vault.config.backlog.file = "tarefas.md".to_string();
        vault.config.backlog.headings = crate::backlog::BacklogHeadings {
            soon: "Em breve".to_string(),
            someday: "Algum dia".to_string(),
            completed: "Concluído".to_string(),
        };
        vault.config.day_planner.heading = "## Hoje".to_string();
        vault.config.language = Some(crate::vault::LanguageConfig {
            tag: Some("pt-BR".to_string()),
            name: Some("Portuguese (Brazil)".to_string()),
        });
        let context = compose_vault_context(&vault, &[], true, None, today());
        assert!(context.contains("`tarefas.md`"));
        assert!(context.contains("`Em breve`, `Algum dia` and `Concluído`"));
        assert!(context.contains("`## Hoje` heading"));
        assert!(context.contains("**Portuguese (Brazil)** (`pt-BR`)"));
        assert!(context.contains("profile.md"));
    }

    #[test]
    fn context_falls_back_to_mirroring_the_person_when_no_language_is_set() {
        let context =
            compose_vault_context(&vault_at("/Users/me/Thock"), &[], false, None, today());
        assert!(context.contains("whatever language the person writes to you in"));
    }

    #[test]
    fn context_lists_installed_routines_with_their_rituals() {
        let manifest = crate::routines::parse_manifest(
            r#"
            schema = 2
            id = "timeline"
            name = "Daily & Weekly"
            version = 1
            summary = "Daily & weekly rhythm."
            doc = "routines/timeline/doc.md"

            [[skill]]
            id = "wrap-today"
            name = "Wrap Today"
            file = "routines/timeline/skills/wrap-today.md"
            summary = "Close out today's note."
            "#,
        )
        .unwrap();
        let context = compose_vault_context(
            &vault_at("/Users/me/Thock"),
            std::slice::from_ref(&manifest),
            false,
            None,
            today(),
        );
        assert!(context.contains("**Daily & Weekly** — `routines/timeline/`"));
        assert!(context.contains("`routines/timeline/doc.md`"));
        assert!(context.contains("Wrap Today (`routines/timeline/skills/wrap-today.md`)"));
    }

    #[test]
    fn context_says_so_when_no_routine_is_installed() {
        let context =
            compose_vault_context(&vault_at("/Users/me/Thock"), &[], false, None, today());
        assert!(context.contains("None are installed yet"));
        assert!(context.contains("skills/thock/new-routine.md"));
    }

    #[test]
    fn context_carries_the_memory_index_when_there_is_one() {
        let index =
            "# What Thock has learned\n\n## People\n- **Ana**, your manager. → people/ana.md";
        let context = compose_vault_context(
            &vault_at("/Users/me/Thock"),
            &[],
            false,
            Some(index),
            today(),
        );
        assert!(context.contains("## What you already know"));
        assert!(context.contains("- **Ana**, your manager. → people/ana.md"));
        assert!(context.contains("`memory/inbox.md`"));
        assert!(context.contains("Never edit `memory/index.md`"));
    }

    #[test]
    fn context_says_nothing_is_known_yet_without_an_index() {
        let context =
            compose_vault_context(&vault_at("/Users/me/Thock"), &[], false, None, today());
        assert!(context.contains("## What you already know"));
        assert!(context.contains("Nothing yet"));
        assert!(context.contains("`memory/inbox.md`"));
    }

    #[test]
    fn gathered_context_reads_the_index_under_the_configured_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = vault_at(dir.path().to_str().unwrap());
        vault.config.memory.index_lines = 2;
        std::fs::create_dir_all(dir.path().join("memory")).unwrap();
        std::fs::write(
            dir.path().join(crate::memory::INDEX_PATH),
            "# Learned\n- one\n- two\n",
        )
        .unwrap();
        let context = gather_vault_context(Some(&vault), today()).unwrap();
        assert!(context.contains("- one"));
        assert!(!context.contains("- two"));
        assert!(context.contains(crate::memory::INDEX_OVER_CAP_LINE));
    }

    #[test]
    fn a_workspace_that_is_not_a_vault_gets_no_context() {
        assert!(gather_vault_context(None, today()).is_none());
    }
}
