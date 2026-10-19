#![allow(
    clippy::disallowed_methods,
    reason = "We are not in an async environment, so std::process::Command is fine"
)]
#![cfg_attr(any(target_os = "linux", target_os = "freebsd"), allow(dead_code))]

mod completions;

use crate::completions::Shell;

use anyhow::{Context as _, Result};
use clap::{CommandFactory, Parser};
use cli::{CliRequest, CliResponse, IpcHandshake, ipc::IpcOneShotServer};
use parking_lot::Mutex;
use std::{
    env,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::Arc,
    thread::{self, JoinHandle},
};
use tempfile::NamedTempFile;
use util::paths::PathWithPosition;

use std::io::IsTerminal;

const URL_PREFIX: [&'static str; 2] = ["thock://", "file://"];

struct Detect;

trait InstalledApp {
    fn version_string(&self) -> String;
    fn launch(&self, ipc_url: String, user_data_dir: Option<&str>) -> anyhow::Result<()>;
    fn run_foreground(
        &self,
        ipc_url: String,
        user_data_dir: Option<&str>,
    ) -> io::Result<ExitStatus>;
    fn path(&self) -> PathBuf;
}

#[derive(Parser, Debug)]
#[command(
    name = "thock",
    disable_version_flag = true,
    before_help = "The Thock command line.
This CLI is a separate binary that opens Thock.

Examples:
    `thock`
          Simply opens Thock
    `thock --foreground`
          Runs in foreground (shows all logs)
    `thock path-to-your-vault`
          Open your vault in Thock
    `thock -n path-to-file `
          Open file/folder in a new window",
    after_help = "To read from stdin, append '-', e.g. 'ps axf | thock -'"
)]
struct Args {
    /// Wait for all of the given paths to be opened/closed before exiting.
    ///
    /// When opening a directory, waits until the created window is closed.
    #[arg(short, long)]
    wait: bool,
    /// Add files to the currently open workspace
    #[arg(short, long, overrides_with_all = ["new", "reuse", "existing", "classic"])]
    add: bool,
    /// Create a new workspace
    #[arg(short, long, overrides_with_all = ["add", "reuse", "existing", "classic"])]
    new: bool,
    /// Reuse an existing window, replacing its workspace
    #[arg(short, long, overrides_with_all = ["add", "new", "existing", "classic"], hide = true)]
    reuse: bool,
    /// Open in existing Thock window
    #[arg(short = 'e', long = "existing", overrides_with_all = ["add", "new", "reuse", "classic"])]
    existing: bool,
    /// Use the classic open behavior: new window for directories, reuse for files
    #[arg(long, hide = true, overrides_with_all = ["add", "new", "reuse", "existing"])]
    classic: bool,
    /// Sets a custom directory for all user data (e.g., database, extensions, logs).
    /// This overrides the default platform-specific data directory location:
    #[cfg_attr(target_os = "macos", doc = "`~/Library/Application Support/Thock`.")]
    #[cfg_attr(not(target_os = "macos"), doc = "`$XDG_DATA_HOME/thock`.")]
    #[arg(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    user_data_dir: Option<String>,
    /// The paths to open in Thock (space-separated).
    ///
    /// Use `path:line:column` syntax to open a file at the given line and column.
    #[arg(trailing_var_arg = true, value_hint = clap::ValueHint::AnyPath)]
    paths_with_position: Vec<String>,
    /// Print Thock's version and the app path.
    #[arg(short, long)]
    version: bool,
    /// Run Thock in the foreground (useful for debugging)
    #[arg(long)]
    foreground: bool,
    /// Custom path to Thock.app or the thock binary
    #[arg(long)]
    app: Option<PathBuf>,
    /// Not supported in the Thock CLI, only on the Thock binary.
    /// Will attempt to give the correct command to run
    #[arg(long)]
    system_specs: bool,
    /// Generate shell completions for Thock
    #[arg(long, value_names = ["SHELL"])]
    completions: Option<Shell>,

    /// Used for SSH/Git password authentication, to remove the need for netcat as a dependency,
    /// by having Thock act like netcat communicating over a Unix socket.
    #[arg(long, hide = true)]
    askpass: Option<String>,
}

/// Parses a path containing a position (e.g. `path:line:column`)
/// and returns its canonicalized string representation.
///
/// If a part of path doesn't exist, it will canonicalize the
/// existing part and append the non-existing part.
///
/// This method must return an absolute path, as many Thock
/// crates assume absolute paths.
fn parse_path_with_position(argument_str: &str) -> anyhow::Result<String> {
    match Path::new(argument_str).canonicalize() {
        Ok(existing_path) => Ok(PathWithPosition::from_path(existing_path)),
        Err(_) => PathWithPosition::parse_str(argument_str).map_path(|mut path| {
            let curdir = env::current_dir().context("retrieving current directory")?;
            let mut children = Vec::new();
            let root;
            loop {
                // canonicalize handles './', and '/'.
                if let Ok(canonicalized) = fs::canonicalize(&path) {
                    root = canonicalized;
                    break;
                }
                // The comparison to `curdir` is just a shortcut
                // since we know it is canonical. The other one
                // is if `argument_str` is a string that starts
                // with a name (e.g. "foo/bar").
                if path == curdir || path == Path::new("") {
                    root = curdir;
                    break;
                }
                children.push(
                    path.file_name()
                        .with_context(|| format!("parsing as path with position {argument_str}"))?
                        .to_owned(),
                );
                if !path.pop() {
                    unreachable!("parsing as path with position {argument_str}");
                }
            }
            Ok(children.iter().rev().fold(root, |mut path, child| {
                path.push(child);
                path
            }))
        }),
    }
    .map(|path_with_pos| path_with_pos.to_string(&|path| path.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use util::path;
    use util::paths::SanitizedPath;
    use util::test::TempTree;

    macro_rules! assert_path_eq {
        ($left:expr, $right:expr) => {
            assert_eq!(
                SanitizedPath::new(Path::new(&$left)),
                SanitizedPath::new(Path::new(&$right))
            )
        };
    }

    fn cwd() -> PathBuf {
        env::current_dir().unwrap()
    }

    static CWD_LOCK: Mutex<()> = Mutex::new(());

    fn with_cwd<T>(path: &Path, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
        let _lock = CWD_LOCK.lock();
        let old_cwd = cwd();
        env::set_current_dir(path)?;
        let result = f();
        env::set_current_dir(old_cwd)?;
        result
    }

    #[test]
    fn test_parse_non_existing_path() {
        // Absolute path
        let result = parse_path_with_position(path!("/non/existing/path.txt")).unwrap();
        assert_path_eq!(result, path!("/non/existing/path.txt"));

        // Absolute path in cwd
        let path = cwd().join(path!("non/existing/path.txt"));
        let expected = path.to_string_lossy().to_string();
        let result = parse_path_with_position(&expected).unwrap();
        assert_path_eq!(result, expected);

        // Relative path
        let result = parse_path_with_position(path!("non/existing/path.txt")).unwrap();
        assert_path_eq!(result, expected)
    }

    #[test]
    fn test_parse_existing_path() {
        let temp_tree = TempTree::new(json!({
            "file.txt": "",
        }));
        let file_path = temp_tree.path().join("file.txt");
        let expected = file_path.to_string_lossy().to_string();

        // Absolute path
        let result = parse_path_with_position(file_path.to_str().unwrap()).unwrap();
        assert_path_eq!(result, expected);

        // Relative path
        let result = with_cwd(temp_tree.path(), || parse_path_with_position("file.txt")).unwrap();
        assert_path_eq!(result, expected);
    }

    // NOTE:
    // While POSIX symbolic links are somewhat supported on Windows, they are an opt in by the user, and thus
    // we assume that they are not supported out of the box.
    #[cfg(not(windows))]
    #[test]
    fn test_parse_symlink_file() {
        let temp_tree = TempTree::new(json!({
            "target.txt": "",
        }));
        let target_path = temp_tree.path().join("target.txt");
        let symlink_path = temp_tree.path().join("symlink.txt");
        std::os::unix::fs::symlink(&target_path, &symlink_path).unwrap();

        // Absolute path
        let result = parse_path_with_position(symlink_path.to_str().unwrap()).unwrap();
        assert_eq!(result, target_path.to_string_lossy());

        // Relative path
        let result =
            with_cwd(temp_tree.path(), || parse_path_with_position("symlink.txt")).unwrap();
        assert_eq!(result, target_path.to_string_lossy());
    }

    #[cfg(not(windows))]
    #[test]
    fn test_parse_symlink_dir() {
        let temp_tree = TempTree::new(json!({
            "some": {
                "dir": { // symlink target
                    "ec": {
                        "tory": {
                            "file.txt": "",
        }}}}}));

        let target_file_path = temp_tree.path().join("some/dir/ec/tory/file.txt");
        let expected = target_file_path.to_string_lossy();

        let dir_path = temp_tree.path().join("some/dir");
        let symlink_path = temp_tree.path().join("symlink");
        std::os::unix::fs::symlink(&dir_path, &symlink_path).unwrap();

        // Absolute path
        let result =
            parse_path_with_position(symlink_path.join("ec/tory/file.txt").to_str().unwrap())
                .unwrap();
        assert_eq!(result, expected);

        // Relative path
        let result = with_cwd(temp_tree.path(), || {
            parse_path_with_position("symlink/ec/tory/file.txt")
        })
        .unwrap();
        assert_eq!(result, expected);
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    #[cfg(unix)]
    util::prevent_root_execution();

    // Must happen before clap — SSH invokes cli.exe directly as SSH_ASKPASS
    // and passes the socket path via env var to avoid argument parsing.
    if let Ok(socket) = std::env::var("ZED_ASKPASS_SOCKET") {
        askpass::main_from_args(&socket, std::env::args().skip(1));
        return Ok(());
    }

    let args = Args::parse();

    // `thock --askpass` makes the CLI operate in nc/netcat mode for use with askpass
    if let Some(socket) = &args.askpass {
        askpass::main(socket);
        return Ok(());
    }

    // Set custom data directory before any path operations
    let user_data_dir = args.user_data_dir.clone();
    if let Some(dir) = &user_data_dir {
        paths::set_custom_data_dir(dir);
    }

    let app = Detect::detect(args.app.as_deref()).context("Bundle detection")?;

    if let Some(shell) = &args.completions {
        let file_path = std::env::current_exe()?;
        let file_name = file_path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or("--completions expects a UTF-8 name for cli bin")
            .map_err(anyhow::Error::msg)?;
        let mut cmd = Args::command();
        cmd.set_bin_name(file_name);
        cmd.build();
        crate::completions::main(&cmd, shell);
        return Ok(());
    }

    if args.version {
        println!("{}", app.version_string());
        return Ok(());
    }

    if args.system_specs {
        let path = app.path();
        let msg = [
            "The `--system-specs` argument is not supported in the Thock CLI, only on the Thock binary.",
            "To retrieve the system specs on the command line, run the following command:",
            &format!("{} --system-specs", path.display()),
        ];
        anyhow::bail!(msg.join("\n"));
    }

    let (server, server_name) =
        IpcOneShotServer::<IpcHandshake>::new().context("Handshake before Thock spawn")?;
    let url = format!("thock-cli://{server_name}");

    let open_behavior = if args.new {
        cli::OpenBehavior::AlwaysNew
    } else if args.add {
        cli::OpenBehavior::Add
    } else if args.existing {
        cli::OpenBehavior::ExistingWindow
    } else if args.classic {
        cli::OpenBehavior::Classic
    } else if args.reuse {
        cli::OpenBehavior::Reuse
    } else {
        cli::OpenBehavior::Default
    };

    let env = {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            use collections::HashMap;

            // On Linux, the desktop entry uses `cli` to spawn `thock`.
            // We need to handle env vars correctly since std::env::vars() may not contain
            // project-specific vars (e.g. those set by direnv).
            // By setting env to None here, the LSP will use worktree env vars instead,
            // which is what we want.
            if !std::io::stdout().is_terminal() {
                None
            } else {
                Some(std::env::vars().collect::<HashMap<_, _>>())
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        {
            use collections::HashMap;

            Some(std::env::vars().collect::<HashMap<_, _>>())
        }
    };

    let exit_status = Arc::new(Mutex::new(None));
    let mut paths = vec![];
    let mut urls = vec![];
    let mut stdin_tmp_file: Option<fs::File> = None;
    let mut anonymous_fd_tmp_files = vec![];

    for path in args.paths_with_position.iter() {
        if URL_PREFIX.iter().any(|&prefix| path.starts_with(prefix)) {
            urls.push(path.to_string());
        } else if path == "-" && args.paths_with_position.len() == 1 {
            let file = NamedTempFile::new()?;
            paths.push(file.path().to_string_lossy().into_owned());
            let (file, _) = file.keep()?;
            stdin_tmp_file = Some(file);
        } else if let Some(file) = anonymous_fd(path) {
            let tmp_file = NamedTempFile::new()?;
            paths.push(tmp_file.path().to_string_lossy().into_owned());
            let (tmp_file, _) = tmp_file.keep()?;
            anonymous_fd_tmp_files.push((file, tmp_file));
        } else {
            paths.push(parse_path_with_position(path)?);
        }
    }

    rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .stack_size(10 * 1024 * 1024)
        .thread_name(|ix| format!("RayonWorker{}", ix))
        .build_global()
        .unwrap();

    let sender: JoinHandle<anyhow::Result<()>> = thread::Builder::new()
        .name("CliReceiver".to_string())
        .spawn({
            let exit_status = exit_status.clone();
            let user_data_dir_for_thread = user_data_dir.clone();
            move || {
                let (_, handshake) = server.accept().context("Handshake after Thock spawn")?;
                let (tx, rx) = (handshake.requests, handshake.responses);

                let open_request = CliRequest::Open {
                    paths,
                    urls,
                    wait: args.wait,
                    open_behavior,
                    env,
                    user_data_dir: user_data_dir_for_thread,
                    cwd: env::current_dir().ok(),
                };

                tx.send(open_request)?;

                while let Ok(response) = rx.recv() {
                    match response {
                        CliResponse::Ping => {}
                        CliResponse::Stdout { message } => println!("{message}"),
                        CliResponse::Stderr { message } => eprintln!("{message}"),
                        CliResponse::Exit { status } => {
                            exit_status.lock().replace(status);
                            return Ok(());
                        }
                        CliResponse::PromptOpenBehavior => {
                            let behavior = prompt_open_behavior()
                                .unwrap_or(cli::CliBehaviorSetting::ExistingWindow);
                            tx.send(CliRequest::SetOpenBehavior { behavior })?;
                        }
                    }
                }

                Ok(())
            }
        })
        .unwrap();

    let stdin_pipe_handle: Option<JoinHandle<anyhow::Result<()>>> =
        stdin_tmp_file.map(|mut tmp_file| {
            thread::Builder::new()
                .name("CliStdin".to_string())
                .spawn(move || {
                    let mut stdin = std::io::stdin().lock();
                    if !io::IsTerminal::is_terminal(&stdin) {
                        io::copy(&mut stdin, &mut tmp_file)?;
                    }
                    Ok(())
                })
                .unwrap()
        });

    let anonymous_fd_pipe_handles: Vec<_> = anonymous_fd_tmp_files
        .into_iter()
        .map(|(mut file, mut tmp_file)| {
            thread::Builder::new()
                .name("CliAnonymousFd".to_string())
                .spawn(move || io::copy(&mut file, &mut tmp_file))
                .unwrap()
        })
        .collect();

    if args.foreground {
        app.run_foreground(url, user_data_dir.as_deref())?;
    } else {
        app.launch(url, user_data_dir.as_deref())?;
        sender.join().unwrap()?;
        if let Some(handle) = stdin_pipe_handle {
            handle.join().unwrap()?;
        }
        for handle in anonymous_fd_pipe_handles {
            handle.join().unwrap()?;
        }
    }

    if let Some(exit_status) = exit_status.lock().take() {
        std::process::exit(exit_status);
    }
    Ok(())
}

fn anonymous_fd(path: &str) -> Option<fs::File> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::{self, FromRawFd};

        let fd_str = path.strip_prefix("/proc/self/fd/")?;

        let link = fs::read_link(path).ok()?;
        if !link.starts_with("memfd:") {
            return None;
        }

        let fd: fd::RawFd = fd_str.parse().ok()?;
        let file = unsafe { fs::File::from_raw_fd(fd) };
        Some(file)
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    {
        use std::os::{
            fd::{self, FromRawFd},
            unix::fs::FileTypeExt,
        };

        let fd_str = path.strip_prefix("/dev/fd/")?;

        let metadata = fs::metadata(path).ok()?;
        let file_type = metadata.file_type();
        if !file_type.is_fifo() && !file_type.is_socket() {
            return None;
        }
        let fd: fd::RawFd = fd_str.parse().ok()?;
        let file = unsafe { fs::File::from_raw_fd(fd) };
        Some(file)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    {
        _ = path;
        // not implemented for other BSDs. Could be, but isn't yet
        None
    }
}

/// Shows an interactive prompt asking the user to choose the default open
/// behavior for `thock <path>`. Returns `None` if the prompt cannot be shown
/// (e.g. stdin is not a terminal) or the user cancels.
fn prompt_open_behavior() -> Option<cli::CliBehaviorSetting> {
    if !std::io::stdin().is_terminal() {
        return None;
    }

    let blue = console::Style::new().blue();
    let items = [
        format!(
            "Add to existing Thock window ({})",
            blue.apply_to("thock --existing")
        ),
        format!("Open a new window ({})", blue.apply_to("thock --classic")),
    ];

    let prompt = format!(
        "Configure default behavior for {}\n{}",
        blue.apply_to("thock <path>"),
        console::style("You can change this later in Thock settings"),
    );

    let selection = dialoguer::Select::new()
        .with_prompt(&prompt)
        .items(&items)
        .default(0)
        .interact()
        .ok()?;

    Some(if selection == 0 {
        cli::CliBehaviorSetting::ExistingWindow
    } else {
        cli::CliBehaviorSetting::NewWindow
    })
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux {
    use std::{
        env,
        ffi::OsString,
        io,
        os::unix::net::{SocketAddr, UnixDatagram},
        path::{Path, PathBuf},
        process::{self, ExitStatus},
        thread,
        time::Duration,
    };

    use anyhow::{Context as _, anyhow};
    use cli::FORCE_CLI_MODE_ENV_VAR_NAME;
    use fork::Fork;

    use crate::{Detect, InstalledApp};

    struct App(PathBuf);

    impl Detect {
        pub fn detect(path: Option<&Path>) -> anyhow::Result<impl InstalledApp> {
            let path = if let Some(path) = path {
                path.to_path_buf().canonicalize()?
            } else {
                let cli = env::current_exe()?;
                let dir = cli.parent().context("no parent path for cli")?;

                // libexec is the standard, lib/thock is for Arch (and other non-libexec distros),
                // ./thock is for the target directory in development builds.
                let possible_locations = [
                    "../libexec/thock-editor",
                    "../lib/thock/thock-editor",
                    "./thock",
                ];
                possible_locations
                    .iter()
                    .find_map(|p| dir.join(p).canonicalize().ok().filter(|path| path != &cli))
                    .with_context(|| {
                        format!("could not find any of: {}", possible_locations.join(", "))
                    })?
            };

            Ok(App(path))
        }
    }

    impl InstalledApp for App {
        fn version_string(&self) -> String {
            format!(
                "Thock {}{}{} – {}",
                if *release_channel::RELEASE_CHANNEL_NAME == "stable" {
                    "".to_string()
                } else {
                    format!("{} ", *release_channel::RELEASE_CHANNEL_NAME)
                },
                option_env!("RELEASE_VERSION").unwrap_or_default(),
                match option_env!("ZED_COMMIT_SHA") {
                    Some(commit_sha) => format!(" {commit_sha} "),
                    None => "".to_string(),
                },
                self.0.display(),
            )
        }

        fn launch(&self, ipc_url: String, user_data_dir: Option<&str>) -> anyhow::Result<()> {
            let data_dir = user_data_dir
                .map(PathBuf::from)
                .unwrap_or_else(|| paths::data_dir().clone());

            let sock_path = data_dir.join(format!(
                "thock-{}.sock",
                *release_channel::RELEASE_CHANNEL_NAME
            ));
            let sock = UnixDatagram::unbound()?;
            if sock.connect(&sock_path).is_err() {
                self.boot_background(ipc_url, user_data_dir)?;
            } else {
                sock.send(ipc_url.as_bytes())?;
            }
            Ok(())
        }

        fn run_foreground(
            &self,
            ipc_url: String,
            user_data_dir: Option<&str>,
        ) -> io::Result<ExitStatus> {
            let mut cmd = std::process::Command::new(self.0.clone());
            cmd.arg(ipc_url);
            if let Some(dir) = user_data_dir {
                cmd.arg("--user-data-dir").arg(dir);
            }
            cmd.status()
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl App {
        fn boot_background(
            &self,
            ipc_url: String,
            user_data_dir: Option<&str>,
        ) -> anyhow::Result<()> {
            let path = &self.0;

            match fork::fork() {
                Ok(Fork::Parent(_)) => Ok(()),
                Ok(Fork::Child) => {
                    unsafe { std::env::set_var(FORCE_CLI_MODE_ENV_VAR_NAME, "") };
                    if fork::setsid().is_err() {
                        eprintln!("failed to setsid: {}", std::io::Error::last_os_error());
                        process::exit(1);
                    }
                    if fork::close_fd().is_err() {
                        eprintln!("failed to close_fd: {}", std::io::Error::last_os_error());
                    }
                    let mut args: Vec<OsString> =
                        vec![path.as_os_str().to_owned(), OsString::from(ipc_url)];
                    if let Some(dir) = user_data_dir {
                        args.push(OsString::from("--user-data-dir"));
                        args.push(OsString::from(dir));
                    }
                    let error = exec::execvp(path.clone(), &args);
                    // if exec succeeded, we never get here.
                    eprintln!("failed to exec {:?}: {}", path, error);
                    process::exit(1)
                }
                Err(_) => Err(anyhow!(io::Error::last_os_error())),
            }
        }

        fn wait_for_socket(
            &self,
            sock_addr: &SocketAddr,
            sock: &mut UnixDatagram,
        ) -> Result<(), std::io::Error> {
            for _ in 0..100 {
                thread::sleep(Duration::from_millis(10));
                if sock.connect_addr(sock_addr).is_ok() {
                    return Ok(());
                }
            }
            sock.connect_addr(sock_addr)
        }
    }
}

#[cfg(target_os = "macos")]
mod mac_os {
    use anyhow::{Context as _, Result};
    use core_foundation::{
        array::{CFArray, CFIndex},
        base::TCFType as _,
        string::kCFStringEncodingUTF8,
        url::{CFURL, CFURLCreateWithBytes},
    };
    use core_services::{
        LSLaunchURLSpec, LSOpenFromURLSpec, kLSLaunchDefaults, kLSLaunchDontSwitch,
    };
    use serde::Deserialize;
    use std::{
        ffi::OsStr,
        fs, io,
        path::{Path, PathBuf},
        process::ExitStatus,
        ptr,
    };

    use cli::FORCE_CLI_MODE_ENV_VAR_NAME;

    use crate::{Detect, InstalledApp};

    #[derive(Debug, Deserialize)]
    struct InfoPlist {
        #[serde(rename = "CFBundleShortVersionString")]
        bundle_short_version_string: String,
    }

    enum Bundle {
        App {
            app_bundle: PathBuf,
            plist: InfoPlist,
        },
        LocalPath {
            executable: PathBuf,
        },
    }

    fn locate_bundle() -> Result<PathBuf> {
        let cli_path = std::env::current_exe()?.canonicalize()?;
        let mut app_path = cli_path.clone();
        while app_path.extension() != Some(OsStr::new("app")) {
            anyhow::ensure!(
                app_path.pop(),
                "cannot find app bundle containing {cli_path:?}"
            );
        }
        Ok(app_path)
    }

    impl Detect {
        pub fn detect(path: Option<&Path>) -> anyhow::Result<impl InstalledApp> {
            let bundle_path = if let Some(bundle_path) = path {
                bundle_path
                    .canonicalize()
                    .with_context(|| format!("Args bundle path {bundle_path:?} canonicalization"))?
            } else {
                locate_bundle().context("bundle autodiscovery")?
            };

            match bundle_path.extension().and_then(|ext| ext.to_str()) {
                Some("app") => {
                    let plist_path = bundle_path.join("Contents/Info.plist");
                    let plist =
                        plist::from_file::<_, InfoPlist>(&plist_path).with_context(|| {
                            format!("Reading *.app bundle plist file at {plist_path:?}")
                        })?;
                    Ok(Bundle::App {
                        app_bundle: bundle_path,
                        plist,
                    })
                }
                _ => Ok(Bundle::LocalPath {
                    executable: bundle_path,
                }),
            }
        }
    }

    impl InstalledApp for Bundle {
        fn version_string(&self) -> String {
            format!("Thock {} – {}", self.version(), self.path().display(),)
        }

        fn launch(&self, url: String, user_data_dir: Option<&str>) -> anyhow::Result<()> {
            match self {
                Self::App { app_bundle, .. } => {
                    let app_path = app_bundle;

                    let status = unsafe {
                        let app_url = CFURL::from_path(app_path, true)
                            .with_context(|| format!("invalid app path {app_path:?}"))?;
                        let url_to_open = CFURL::wrap_under_create_rule(CFURLCreateWithBytes(
                            ptr::null(),
                            url.as_ptr(),
                            url.len() as CFIndex,
                            kCFStringEncodingUTF8,
                            ptr::null(),
                        ));
                        // equivalent to: open thock-cli:... -a /Applications/Thock.app
                        let urls_to_open =
                            CFArray::from_copyable(&[url_to_open.as_concrete_TypeRef()]);
                        LSOpenFromURLSpec(
                            &LSLaunchURLSpec {
                                appURL: app_url.as_concrete_TypeRef(),
                                itemURLs: urls_to_open.as_concrete_TypeRef(),
                                passThruParams: ptr::null(),
                                launchFlags: kLSLaunchDefaults | kLSLaunchDontSwitch,
                                asyncRefCon: ptr::null_mut(),
                            },
                            ptr::null_mut(),
                        )
                    };

                    anyhow::ensure!(
                        status == 0,
                        "cannot start app bundle {}",
                        self.version_string()
                    );
                }

                Self::LocalPath { executable, .. } => {
                    let executable_parent = executable
                        .parent()
                        .with_context(|| format!("Executable {executable:?} path has no parent"))?;
                    let subprocess_stdout_file = fs::File::create(
                        executable_parent.join("thock_dev.log"),
                    )
                    .with_context(|| format!("Log file creation in {executable_parent:?}"))?;
                    let subprocess_stdin_file =
                        subprocess_stdout_file.try_clone().with_context(|| {
                            format!("Cloning descriptor for file {subprocess_stdout_file:?}")
                        })?;
                    let mut command = std::process::Command::new(executable);
                    command.env(FORCE_CLI_MODE_ENV_VAR_NAME, "");
                    if let Some(dir) = user_data_dir {
                        command.arg("--user-data-dir").arg(dir);
                    }
                    command
                        .stderr(subprocess_stdout_file)
                        .stdout(subprocess_stdin_file)
                        .arg(url);

                    command
                        .spawn()
                        .with_context(|| format!("Spawning {command:?}"))?;
                }
            }

            Ok(())
        }

        fn run_foreground(
            &self,
            ipc_url: String,
            user_data_dir: Option<&str>,
        ) -> io::Result<ExitStatus> {
            let path = match self {
                Bundle::App { app_bundle, .. } => app_bundle.join("Contents/MacOS/thock"),
                Bundle::LocalPath { executable, .. } => executable.clone(),
            };

            let mut cmd = std::process::Command::new(path);
            cmd.arg(ipc_url);
            if let Some(dir) = user_data_dir {
                cmd.arg("--user-data-dir").arg(dir);
            }
            cmd.status()
        }

        fn path(&self) -> PathBuf {
            match self {
                Bundle::App { app_bundle, .. } => app_bundle.join("Contents/MacOS/thock"),
                Bundle::LocalPath { executable, .. } => executable.clone(),
            }
        }
    }

    impl Bundle {
        fn version(&self) -> String {
            match self {
                Self::App { plist, .. } => plist.bundle_short_version_string.clone(),
                Self::LocalPath { .. } => "<development>".to_string(),
            }
        }

        fn path(&self) -> &Path {
            match self {
                Self::App { app_bundle, .. } => app_bundle,
                Self::LocalPath { executable, .. } => executable,
            }
        }
    }
}
