//! The hosted Thock Agent's harness (V25 §5): Pi, launched over ACP through
//! the `pi-acp` adapter, both installed with the managed Node runtime into a
//! Thock-owned folder. The per-user gateway key, the model behind the
//! requested tier, and the note-taking system prompt reach Pi through a
//! private Pi config directory and the process environment, so nothing is
//! read from or written to the user's own `~/.pi`.

use agent_servers::AcpConnection;
use anyhow::{Context as _, Result};
use collections::HashMap;
use fs::Fs;
use gpui::{AsyncApp, Entity};
use node_runtime::{NodeRuntime, VersionStrategy};
use project::Project;
use project::agent_server_store::{AgentId, AgentServerCommand};
use semver::Version;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use crate::agent::ModelTier;

/// The ACP adapter and the harness it drives, pinned so a registry or npm
/// change never reaches users untested (spec §6: `pi-acp` is a one-person
/// MVP and Thock must be ready to fork it).
pub const PI_ACP_PACKAGE: &str = "pi-acp";
pub const PI_ACP_VERSION: &str = "0.0.33";
pub const PI_PACKAGE: &str = "@earendil-works/pi-coding-agent";
pub const PI_VERSION: &str = "0.85.1";
pub const AGENT_ID: &str = "thock-hosted-agent";

/// A note-taking prompt in place of Pi's coding one. Pi appends context
/// files (the vault's `AGENTS.md`) after it.
pub const SYSTEM_PROMPT: &str = include_str!("../assets/hosted-agent/SYSTEM.md");

/// Where the npm packages land, beside Zed's own registry-installed agents.
pub fn install_dir() -> PathBuf {
    paths::external_agents_dir().join("thock-hosted")
}

/// Pi's config directory for the hosted path, one per tier so two sessions
/// on different tiers never race over one `settings.json`.
pub fn pi_config_dir(tier: ModelTier) -> PathBuf {
    paths::data_dir()
        .join("thock")
        .join("hosted-agent")
        .join(tier.as_str())
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
        node.npm_install_packages(
            &dir,
            &[(PI_ACP_PACKAGE, PI_ACP_VERSION), (PI_PACKAGE, PI_VERSION)],
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

/// Writes the tier's Pi config directory and returns it.
pub async fn write_pi_config(fs: &Arc<dyn Fs>, tier: ModelTier, model_id: &str) -> Result<PathBuf> {
    let dir = pi_config_dir(tier);
    fs.create_dir(&dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;
    fs.atomic_write(dir.join("settings.json"), pi_settings(model_id))
        .await
        .context("writing the Thock Agent settings")?;
    fs.atomic_write(dir.join("SYSTEM.md"), SYSTEM_PROMPT.to_string())
        .await
        .context("writing the Thock Agent prompt")?;
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
    let pi_config_dir = write_pi_config(&fs, tier, model_id).await?;
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
    fn config_dirs_are_per_tier() {
        assert_ne!(
            pi_config_dir(ModelTier::Default),
            pi_config_dir(ModelTier::Fast)
        );
        assert!(!SYSTEM_PROMPT.trim().is_empty());
    }
}
