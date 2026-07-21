use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::thread;
use std::time::Duration;

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROTOCOL_VERSION: &str = "1";

const DAEMON_DIRECTORY: &str = "lighter";
const SOCKET_FILE: &str = "daemon.sock";
const LOCK_FILE: &str = "daemon.lock";
const HEADER_TERMINATOR: u8 = b'\n';
const STOP_LANGUAGE: &str = "lighter-internal-stop";
const CLIENT_REQUEST_ID: u64 = 1;
const DAEMON_SERVE_ARGUMENTS: [&str; 2] = ["daemon", "serve"];
const STARTUP_ATTEMPTS: usize = 250;
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(20);

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
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Subcommand)]
pub enum DaemonAction {
    /// Spawn the background daemon.
    Spawn,
    /// Kill the background daemon.
    Kill,
    #[command(hide = true)]
    Serve,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct RequestHeader {
    version: String,
    id: u64,
    lang: String,
    length: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct ResponseHeader {
    version: String,
    length: usize,
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
struct Request {
    header: RequestHeader,
    source: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
struct Response {
    header: ResponseHeader,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct DaemonPaths {
    directory: PathBuf,
    endpoint: PathBuf,
    lock: PathBuf,
}

impl DaemonPaths {
    pub fn discover() -> Self {
        let directory = runtime_directory().join(DAEMON_DIRECTORY);
        Self {
            endpoint: directory.join(SOCKET_FILE),
            lock: directory.join(LOCK_FILE),
            directory,
        }
    }

    #[cfg(test)]
    fn in_directory(directory: &Path) -> Self {
        Self {
            endpoint: directory.join(SOCKET_FILE),
            lock: directory.join(LOCK_FILE),
            directory: directory.to_owned(),
        }
    }
}

fn runtime_directory() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("{DAEMON_DIRECTORY}-{}", runtime_identity()))
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

fn write_header(mut output: impl Write, header: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut output, header)?;
    output.write_all(&[HEADER_TERMINATOR])?;
    Ok(())
}

fn read_header<T: serde::de::DeserializeOwned>(input: &mut impl BufRead) -> Result<Option<T>> {
    let mut bytes = Vec::new();
    match input.read_until(HEADER_TERMINATOR, &mut bytes)? {
        0 => Ok(None),
        _ => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(Error::from),
    }
}

fn write_request(mut output: impl Write, id: u64, language: &str, source: &[u8]) -> Result<()> {
    write_header(
        &mut output,
        &RequestHeader {
            version: PROTOCOL_VERSION.to_owned(),
            id,
            lang: language.to_owned(),
            length: source.len(),
        },
    )?;
    output.write_all(source)?;
    output.flush()?;
    Ok(())
}

fn read_request(input: &mut impl BufRead) -> Result<Option<Request>> {
    let Some(header) = read_header::<RequestHeader>(input)? else {
        return Ok(None);
    };
    let mut source = vec![0; header.length];
    input.read_exact(&mut source)?;
    Ok(Some(Request { header, source }))
}

fn write_response(mut output: impl Write, id: u64, response: &str) -> Result<()> {
    write_header(
        &mut output,
        &ResponseHeader {
            version: PROTOCOL_VERSION.to_owned(),
            length: response.len(),
            id,
            error: None,
        },
    )?;
    output.write_all(response.as_bytes())?;
    output.flush()?;
    Ok(())
}

fn write_error_response(mut output: impl Write, id: u64, error: &str) -> Result<()> {
    write_header(
        &mut output,
        &ResponseHeader {
            version: PROTOCOL_VERSION.to_owned(),
            length: 0,
            id,
            error: Some(error.to_owned()),
        },
    )?;
    output.flush()?;
    Ok(())
}

fn read_response(input: &mut impl BufRead) -> Result<Response> {
    let header = read_header::<ResponseHeader>(input)?.ok_or_else(|| {
        Error::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "daemon closed before sending a response",
        ))
    })?;
    let mut body = vec![0; header.length];
    input.read_exact(&mut body)?;
    Ok(Response { header, body })
}

fn response_result(response: Response, request_id: u64) -> Result<String> {
    match response.header.version.as_str() {
        PROTOCOL_VERSION => {}
        _ => return Err(Error::UnsupportedVersion(response.header.version)),
    }
    match response.header.id == request_id {
        true => {}
        false => {
            return Err(Error::ResponseId {
                expected: request_id,
                actual: response.header.id,
            });
        }
    }
    match response.header.error {
        Some(error) if response.header.length == 0 => Err(Error::Response(error)),
        Some(_) => Err(Error::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "error response has a non-zero body length",
        ))),
        None => String::from_utf8(response.body).map_err(Error::from),
    }
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

fn handle_request<F, E>(request: Request, mut output: impl Write, highlight: &mut F) -> Result<bool>
where
    F: FnMut(&str, &str) -> std::result::Result<String, E>,
    E: std::fmt::Display,
{
    let id = request.header.id;
    match (
        request.header.version.as_str(),
        request.header.lang.as_str(),
    ) {
        (PROTOCOL_VERSION, STOP_LANGUAGE) => {
            write_response(&mut output, id, "")?;
            return Ok(true);
        }
        (PROTOCOL_VERSION, _) => {}
        (version, _) => {
            write_error_response(
                &mut output,
                id,
                &Error::UnsupportedVersion(version.to_owned()).to_string(),
            )?;
            return Ok(false);
        }
    }

    let source = match String::from_utf8(request.source) {
        Ok(source) => source,
        Err(error) => {
            write_error_response(&mut output, id, &Error::InvalidUtf8(error).to_string())?;
            return Ok(false);
        }
    };
    match highlight(&request.header.lang, &source) {
        Ok(response) => write_response(&mut output, id, &response)?,
        Err(error) => write_error_response(&mut output, id, &error.to_string())?,
    }
    Ok(false)
}

fn serve_connection<F, E>(stream: &mut local::Stream, highlight: &mut F) -> Result<bool>
where
    F: FnMut(&str, &str) -> std::result::Result<String, E>,
    E: std::fmt::Display,
{
    let request = read_request(&mut BufReader::new(&mut *stream))?;
    match request {
        Some(request) => handle_request(request, stream, highlight),
        None => Ok(false),
    }
}

/// Acquire the singleton lock and serve highlight requests until a stop request arrives.
pub fn serve<F, E>(paths: &DaemonPaths, mut highlight: F) -> Result<()>
where
    F: FnMut(&str, &str) -> std::result::Result<String, E>,
    E: std::fmt::Display,
{
    local::prepare_directory(&paths.directory).map_err(Error::RuntimeDirectory)?;
    let _lock = local::acquire_lock(&paths.lock)?;
    let listener = local::bind(paths).map_err(Error::Bind)?;
    let _endpoint_guard = EndpointGuard(&paths.endpoint);

    listener
        .incoming()
        .find_map(|connection| match connection {
            Err(error) => Some(Err(Error::Io(error))),
            Ok(mut stream) => match serve_connection(&mut stream, &mut highlight) {
                Ok(true) => Some(Ok(())),
                Ok(false) | Err(_) => None,
            },
        })
        .unwrap_or(Ok(()))
}

fn unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

fn exchange(paths: &DaemonPaths, language: &str, source: &str) -> Result<Option<String>> {
    let mut stream = match local::connect(paths) {
        Ok(stream) => stream,
        Err(error) if unavailable(&error) => return Ok(None),
        Err(error) => return Err(Error::Io(error)),
    };
    write_request(&mut stream, CLIENT_REQUEST_ID, language, source.as_bytes())?;
    let response = read_response(&mut BufReader::new(stream))?;
    response_result(response, CLIENT_REQUEST_ID).map(Some)
}

/// Use the daemon when it is running, or return `None` when no daemon is available.
pub fn highlight(paths: &DaemonPaths, language: &str, source: &str) -> Result<Option<String>> {
    exchange(paths, language, source)
}

pub fn is_running(paths: &DaemonPaths) -> bool {
    local::connect(paths).is_ok()
}

fn kill(paths: &DaemonPaths) -> Result<()> {
    match exchange(paths, STOP_LANGUAGE, "")? {
        Some(_) => Ok(()),
        None => Err(Error::NotRunning),
    }
}

#[cfg(unix)]
fn detach() {
    let _ = unsafe { libc::setsid() };
}

#[cfg(windows)]
fn detach() {}

fn spawn(paths: &DaemonPaths) -> Result<()> {
    if is_running(paths) {
        return Err(Error::AlreadyRunning);
    }

    let executable = std::env::current_exe().map_err(Error::CurrentExecutable)?;
    let mut command = Command::new(executable);
    command
        .args(DAEMON_SERVE_ARGUMENTS)
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
            if is_running(paths) {
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

fn run_server(paths: &DaemonPaths) -> Result<()> {
    detach();
    let highlighter = crate::Highlighter::default();
    serve(paths, |language, source| {
        highlighter.highlight(crate::Input {
            source,
            path: None,
            lang: crate::LangName::from(language),
        })
    })
}

/// Execute a daemon lifecycle subcommand.
pub fn run(action: DaemonAction) -> Result<()> {
    let paths = DaemonPaths::discover();
    match action {
        DaemonAction::Spawn => spawn(&paths),
        DaemonAction::Kill => kill(&paths),
        DaemonAction::Serve => run_server(&paths),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST_ID: u64 = 42;
    const LANGUAGE: &str = "python";
    const SOURCE: &str = "print('😀')";
    const OUTPUT: &str = "highlighted 😀";
    const HIGHLIGHT_ERROR: &str = "highlight failed";

    fn encoded_request(version: &str, id: u64, language: &str, source: &[u8]) -> Vec<u8> {
        let header = RequestHeader {
            version: version.to_owned(),
            id,
            lang: language.to_owned(),
            length: source.len(),
        };
        let mut request = serde_json::to_vec(&header).unwrap();
        request.push(HEADER_TERMINATOR);
        request.extend(source);
        request
    }

    fn read_encoded_response(bytes: Vec<u8>) -> Response {
        read_response(&mut BufReader::new(io::Cursor::new(bytes))).unwrap()
    }

    #[test]
    fn request_uses_json_header_and_exact_utf8_byte_length() {
        let mut bytes = Vec::new();

        write_request(&mut bytes, REQUEST_ID, LANGUAGE, SOURCE.as_bytes()).unwrap();

        let mut input = BufReader::new(io::Cursor::new(bytes));
        let request = read_request(&mut input).unwrap().unwrap();
        assert_eq!(request.header.version, PROTOCOL_VERSION);
        assert_eq!(request.header.id, REQUEST_ID);
        assert_eq!(request.header.lang, LANGUAGE);
        assert_eq!(request.header.length, SOURCE.len());
        assert_eq!(request.source, SOURCE.as_bytes());
    }

    #[test]
    fn response_repeats_version_and_request_id() {
        let request = Request {
            header: RequestHeader {
                version: PROTOCOL_VERSION.to_owned(),
                id: REQUEST_ID,
                lang: LANGUAGE.to_owned(),
                length: SOURCE.len(),
            },
            source: SOURCE.as_bytes().to_vec(),
        };
        let mut output = Vec::new();

        let stop = handle_request(request, &mut output, &mut |_language, _source| {
            Ok::<_, std::convert::Infallible>(OUTPUT.to_owned())
        })
        .unwrap();

        let response = read_encoded_response(output);
        assert!(!stop);
        assert_eq!(response.header.version, PROTOCOL_VERSION);
        assert_eq!(response.header.id, REQUEST_ID);
        assert_eq!(response.header.length, OUTPUT.len());
        assert_eq!(response.header.error, None);
        assert_eq!(response.body, OUTPUT.as_bytes());
    }

    #[test]
    fn highlight_error_has_no_body() {
        let input = encoded_request(PROTOCOL_VERSION, REQUEST_ID, LANGUAGE, SOURCE.as_bytes());
        let request = read_request(&mut BufReader::new(io::Cursor::new(input)))
            .unwrap()
            .unwrap();
        let mut output = Vec::new();

        handle_request(request, &mut output, &mut |_language, _source| {
            Err::<String, _>(HIGHLIGHT_ERROR)
        })
        .unwrap();

        let response = read_encoded_response(output);
        assert_eq!(response.header.length, 0);
        assert_eq!(response.header.error.as_deref(), Some(HIGHLIGHT_ERROR));
        assert!(response.body.is_empty());
    }

    #[test]
    fn unsupported_version_returns_an_error_with_the_request_id() {
        const UNSUPPORTED_VERSION: &str = "2";
        let input = encoded_request(UNSUPPORTED_VERSION, REQUEST_ID, LANGUAGE, SOURCE.as_bytes());
        let request = read_request(&mut BufReader::new(io::Cursor::new(input)))
            .unwrap()
            .unwrap();
        let mut output = Vec::new();

        handle_request(request, &mut output, &mut |_language, _source| {
            Ok::<_, std::convert::Infallible>(OUTPUT.to_owned())
        })
        .unwrap();

        let response = read_encoded_response(output);
        assert_eq!(response.header.id, REQUEST_ID);
        assert_eq!(response.header.length, 0);
        assert!(
            response
                .header
                .error
                .is_some_and(|error| error.contains(UNSUPPORTED_VERSION))
        );
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
}
