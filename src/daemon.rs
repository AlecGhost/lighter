use std::cell::RefCell;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use arborium::theme::Theme;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{HighlightOptions, Highlighter, Input, LangName, LineRange, Output, logging, lsp};

mod protocol;

const CLIENT_REQUEST_ID: u64 = 1;
const STARTUP_ATTEMPTS: usize = 250;
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaemonPath {
    Directory,
    Endpoint,
    Lock,
}

impl DaemonPath {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Directory => "lighter",
            Self::Endpoint => "daemon.sock",
            Self::Lock => "daemon.lock",
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Daemon I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("Failed to prepare the daemon runtime directory: {0}")]
    RuntimeDirectory(#[source] io::Error),
    #[error("Failed to acquire the daemon lock: {0}")]
    Lock(#[source] io::Error),
    #[error("Failed to bind the daemon IPC endpoint: {0}")]
    Bind(#[source] io::Error),
    #[error("Daemon protocol header is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Daemon protocol version '{0}' is not supported")]
    UnsupportedVersion(String),
    #[error("Daemon protocol body is not valid UTF-8: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
    #[error("Daemon response id {actual} does not match request id {expected}")]
    ResponseId { expected: u64, actual: u64 },
    #[error("Daemon returned an error: {0}")]
    Response(String),
    #[error("Daemon is already running")]
    AlreadyRunning,
    #[error("Daemon is not running")]
    NotRunning,
    #[error("Failed to locate the lighter executable")]
    CurrentExecutable(#[source] io::Error),
    #[error("Failed to spawn the daemon process")]
    Spawn(#[source] io::Error),
    #[error("Daemon exited during startup with status {0}")]
    Startup(process::ExitStatus),
    #[error("Daemon did not become ready in time")]
    StartupTimeout,
    #[error("Failed to load request configuration: {0}")]
    Configuration(String),
    #[error(transparent)]
    Highlight(#[from] crate::Error),
}

#[derive(Clone, Debug)]
pub struct Options {
    pub commands: lsp::Commands,
    pub general_mapping: lsp::CaptureMapping,
    pub lang_mapping: lsp::LangCaptureMapping,
    pub theme: Theme,
    pub format: Output,
    pub log: logging::LogLevel,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RequestOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<LineRange>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_tree_sitter: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_lsp: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Output>,
}

fn is_false(value: &bool) -> bool {
    !value
}

struct Session {
    theme: Theme,
    format: Output,
    log: logging::LogLevel,
    registry: Rc<RefCell<lsp::ServerRegistry>>,
}

impl Session {
    fn new(options: Options) -> Self {
        let registry = lsp::ServerRegistry::new(
            options.commands,
            options.general_mapping,
            options.lang_mapping,
            options.log,
        );
        Self {
            theme: options.theme,
            format: options.format,
            log: options.log,
            registry: Rc::new(RefCell::new(registry)),
        }
    }

    fn highlight(
        &mut self,
        language: &str,
        source: &str,
        request: &RequestOptions,
    ) -> Result<String> {
        Highlighter::with_options(
            Rc::clone(&self.registry),
            HighlightOptions {
                output: request.format.unwrap_or(self.format),
                lsp: !request.no_lsp,
                tree_sitter: !request.no_tree_sitter,
                theme: self.theme.clone(),
                lines: request.lines,
                project: request.project.clone(),
            },
            self.log,
        )
        .highlight(Input {
            source,
            path: None,
            lang: LangName::from(language),
        })
        .map_err(Error::from)
    }
}

#[derive(Debug, Clone)]
struct DaemonPaths {
    directory: PathBuf,
    endpoint: PathBuf,
    lock: PathBuf,
}

impl DaemonPaths {
    fn discover() -> Self {
        let directory = runtime_directory().join(DaemonPath::Directory.as_str());
        Self {
            endpoint: directory.join(DaemonPath::Endpoint.as_str()),
            lock: directory.join(DaemonPath::Lock.as_str()),
            directory,
        }
    }

    #[cfg(test)]
    fn in_directory(directory: &Path) -> Self {
        Self {
            endpoint: directory.join(DaemonPath::Endpoint.as_str()),
            lock: directory.join(DaemonPath::Lock.as_str()),
            directory: directory.to_owned(),
        }
    }
}

fn runtime_directory() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "{}-{}",
                DaemonPath::Directory.as_str(),
                runtime_identity()
            ))
        })
}

#[cfg(unix)]
fn runtime_identity() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(windows)]
fn runtime_identity() -> String {
    std::env::var("USERNAME").unwrap_or_else(|_| "user".to_owned())
}

#[cfg(unix)]
mod local {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};

    pub type Listener = UnixListener;
    pub type Stream = UnixStream;

    pub fn prepare_directory(path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }

    pub fn bind(paths: &DaemonPaths) -> io::Result<Listener> {
        match fs::remove_file(&paths.endpoint) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        UnixListener::bind(&paths.endpoint)
    }

    pub fn connect(paths: &DaemonPaths) -> io::Result<Stream> {
        UnixStream::connect(&paths.endpoint)
    }

    pub fn acquire_lock(path: &Path) -> Result<File> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = io::Error::last_os_error();
            return match error.kind() {
                io::ErrorKind::WouldBlock => Err(Error::AlreadyRunning),
                _ => Err(Error::Lock(error)),
            };
        }
        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        file.flush()?;
        Ok(file)
    }
}

#[cfg(windows)]
mod local {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::os::windows::fs::OpenOptionsExt;

    pub type Listener = TcpListener;
    pub type Stream = TcpStream;

    pub fn prepare_directory(path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    pub fn bind(paths: &DaemonPaths) -> io::Result<Listener> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        fs::write(&paths.endpoint, listener.local_addr()?.to_string())?;
        Ok(listener)
    }

    pub fn connect(paths: &DaemonPaths) -> io::Result<Stream> {
        TcpStream::connect(fs::read_to_string(&paths.endpoint)?)
    }

    pub fn acquire_lock(path: &Path) -> Result<File> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .share_mode(0)
            .write(true)
            .open(path)
            .map_err(|error| match error.kind() {
                io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied => {
                    Error::AlreadyRunning
                }
                _ => Error::Lock(error),
            })
    }
}

struct EndpointGuard<'a>(&'a Path);

impl Drop for EndpointGuard<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

/// Acquire the singleton lock and serve highlight requests until a stop request arrives.
pub fn serve(options: Options) -> Result<()> {
    detach();
    let paths = DaemonPaths::discover();
    local::prepare_directory(&paths.directory).map_err(Error::RuntimeDirectory)?;
    let _lock = local::acquire_lock(&paths.lock)?;
    let listener = local::bind(&paths).map_err(Error::Bind)?;
    let _endpoint_guard = EndpointGuard(&paths.endpoint);
    let mut session = Session::new(options);
    let mut highlight = |language: &str, source: &str, request: &RequestOptions| {
        session.highlight(language, source, request)
    };

    listener
        .incoming()
        .find_map(|connection| match connection {
            Err(error) => Some(Err(Error::Io(error))),
            Ok(mut stream) => match protocol::serve_connection(&mut stream, &mut highlight) {
                Ok(true) => Some(Ok(())),
                Ok(false) | Err(_) => None,
            },
        })
        .unwrap_or(Ok(()))
}

fn exchange(language: &str, source: &str, options: &RequestOptions) -> Result<String> {
    let mut stream = local::connect(&DaemonPaths::discover()).map_err(Error::Io)?;
    protocol::exchange(&mut stream, CLIENT_REQUEST_ID, language, source, options)
}

/// Send a highlight request to the running daemon.
pub fn highlight(language: &str, source: &str, options: &RequestOptions) -> Result<String> {
    exchange(language, source, options)
}

fn running_at(paths: &DaemonPaths) -> bool {
    local::connect(paths).is_ok()
}

pub fn is_running() -> bool {
    running_at(&DaemonPaths::discover())
}

pub fn kill() -> Result<()> {
    match is_running() {
        true => exchange(protocol::STOP_LANGUAGE, "", &RequestOptions::default()).map(|_| ()),
        false => Err(Error::NotRunning),
    }
}

#[cfg(unix)]
fn detach() {
    let _ = unsafe { libc::setsid() };
}

#[cfg(windows)]
fn detach() {}

pub fn spawn(arguments: &[OsString]) -> Result<()> {
    let paths = DaemonPaths::discover();
    if running_at(&paths) {
        return Err(Error::AlreadyRunning);
    }

    let executable = std::env::current_exe().map_err(Error::CurrentExecutable)?;
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    let mut child = command.spawn().map_err(Error::Spawn)?;

    (0..STARTUP_ATTEMPTS)
        .find_map(|_| {
            if running_at(&paths) {
                return Some(Ok(()));
            }
            match child.try_wait() {
                Ok(Some(status)) => Some(Err(Error::Startup(status))),
                Ok(None) => {
                    thread::sleep(STARTUP_POLL_INTERVAL);
                    None
                }
                Err(error) => Some(Err(Error::Spawn(error))),
            }
        })
        .unwrap_or(Err(Error::StartupTimeout))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANGUAGE: &str = "rust";
    const SOURCE: &str = "first\nVec<Span>\n";

    fn options() -> Options {
        Options {
            commands: lsp::default_commands(),
            general_mapping: lsp::CaptureMapping::new(),
            lang_mapping: lsp::LangCaptureMapping::new(),
            theme: arborium_theme::builtin::catppuccin_mocha(),
            format: Output::Ansi,
            log: logging::LogLevel::default(),
        }
    }

    #[test]
    fn lock_allows_only_one_daemon() {
        let directory = tempfile::tempdir().unwrap();
        let paths = DaemonPaths::in_directory(directory.path());
        local::prepare_directory(&paths.directory).unwrap();
        let _lock = local::acquire_lock(&paths.lock).unwrap();

        let error = local::acquire_lock(&paths.lock).unwrap_err();

        assert!(matches!(error, Error::AlreadyRunning));
    }

    #[test]
    fn session_applies_request_options_to_each_highlight() {
        let mut session = Session::new(options());
        let request = RequestOptions {
            no_lsp: true,
            no_tree_sitter: true,
            ..Default::default()
        };
        let first = RequestOptions {
            lines: Some("1:1".parse().unwrap()),
            ..request.clone()
        };
        let second = RequestOptions {
            lines: Some("2:2".parse().unwrap()),
            format: Some(Output::Html),
            ..request
        };

        let first_output = session.highlight(LANGUAGE, SOURCE, &first).unwrap();
        let second_output = session.highlight(LANGUAGE, SOURCE, &second).unwrap();

        assert_eq!(first_output, "first");
        assert_eq!(second_output, "Vec&lt;Span&gt;");
    }
}
