mod rpc;

use lsp_types::notification::{DidCloseTextDocument, DidOpenTextDocument, Exit, Initialized};
use lsp_types::request::{Initialize, SemanticTokensFullRequest, Shutdown};
use lsp_types::{
    ClientCapabilities, DidCloseTextDocumentParams, DidOpenTextDocumentParams, InitializeParams,
    InitializedParams, PartialResultParams, SemanticToken, SemanticTokenModifier,
    SemanticTokenType, SemanticTokensClientCapabilities, SemanticTokensClientCapabilitiesRequests,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, TextDocumentClientCapabilities, TextDocumentIdentifier,
    TextDocumentItem, TokenFormat, Uri, WindowClientCapabilities, WorkDoneProgressParams,
    WorkspaceClientCapabilities, WorkspaceFolder,
};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use thiserror::Error;

use crate::logging::LogLevel;

pub type LangName = Rc<str>;

const FILE_URI_PREFIX: &str = "file://";
const TEMP_FILE_PREFIX: &str = "lighter";
const DEFAULT_DOCUMENT_EXTENSION: &str = "txt";
const MAX_TEMP_FILE_ATTEMPTS: usize = 100;
const URI_HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";
const DOCUMENT_OPEN_WAIT_TIMEOUT: Duration = Duration::from_secs(1);
const PROGRESS_END_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

static NEXT_TEMP_FILE_ID: AtomicUsize = AtomicUsize::new(0);

#[derive(Error, Debug)]
pub enum Error {
    #[error("No language server available for {0}")]
    NoServer(String),
    #[error("Failed to start server for {0}: {1}")]
    FailedServerCommand(String, #[source] std::io::Error),
    #[error("Language server I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Could not create a file URI for document path '{0}'")]
    InvalidDocumentUri(PathBuf),
    #[error("Project path is not a directory: '{0}'")]
    InvalidProjectDirectory(PathBuf),
    #[error(transparent)]
    Rpc(#[from] rpc::Error),
}

type Result<T> = std::result::Result<T, Error>;

/// A single LSP server entry.
#[derive(Debug, Clone)]
pub struct CommandEntry {
    /// The executable name or path (e.g. "rust-analyzer").
    pub command: String,
    /// Arguments passed to the server (e.g. `["--stdio"]`).
    pub args: Vec<String>,
}

/// Default server commands
pub fn default_commands() -> HashMap<LangName, CommandEntry> {
    let entries: &[(&str, &str, &[&str])] = &[
        ("rust", "rust-analyzer", &[]),
        ("python", "basedpyright-langserver", &["--stdio"]),
        ("typescript", "typescript-language-server", &["--stdio"]),
        ("javascript", "typescript-language-server", &["--stdio"]),
        ("tsx", "typescript-language-server", &["--stdio"]),
        ("jsx", "typescript-language-server", &["--stdio"]),
        ("c", "clangd", &[]),
        ("cpp", "clangd", &[]),
        ("go", "gopls", &[]),
        ("java", "jdtls", &[]),
        ("lua", "lua-language-server", &[]),
        ("zig", "zls", &[]),
        ("ruby", "solargraph", &["stdio"]),
        ("html", "vscode-html-language-server", &["--stdio"]),
        ("css", "vscode-css-language-server", &["--stdio"]),
        ("json", "vscode-json-language-server", &["--stdio"]),
        ("toml", "taplo", &["lsp", "stdio"]),
        ("yaml", "yaml-language-server", &["--stdio"]),
        ("bash", "bash-language-server", &["start"]),
        ("kotlin", "kotlin-language-server", &[]),
        ("swift", "sourcekit-lsp", &[]),
        ("elixir", "elixir-ls", &[]),
        ("haskell", "haskell-language-server-wrapper", &["--lsp"]),
        ("ocaml", "ocamllsp", &[]),
        ("latex", "texlab", &[]),
        ("dart", "dart", &["language-server", "--protocol=lsp"]),
        ("csharp", "OmniSharp", &["--languageserver"]),
    ];

    entries
        .iter()
        .map(|(lang, cmd, args)| {
            (
                Rc::from(*lang),
                CommandEntry {
                    command: (*cmd).to_string(),
                    args: args.iter().map(|a| (*a).to_string()).collect(),
                },
            )
        })
        .collect()
}

#[derive(Debug, Default)]
pub struct ServerRegistry {
    clients: HashMap<LangName, Client>,
    commands: HashMap<LangName, CommandEntry>,
    project: Option<Project>,
    log: LogLevel,
}

impl ServerRegistry {
    pub fn new(
        commands: HashMap<LangName, CommandEntry>,
        project: Option<&Path>,
        log: LogLevel,
    ) -> Result<Self> {
        Ok(ServerRegistry {
            commands,
            project: project.map(Project::new).transpose()?,
            log,
            ..Default::default()
        })
    }
}

impl<'a> ServerRegistry {
    pub fn get_server(&'a mut self, lang: LangName) -> Result<Server<'a>> {
        if !self.clients.contains_key(&lang) {
            if let Some(command_entry) = self.commands.get(&lang) {
                let client = Client::new(command_entry, &lang, self.project.as_ref(), self.log)?;
                self.clients.insert(lang.clone(), client);
            } else {
                return Err(Error::NoServer(lang.to_string()));
            }
        }
        let client = self.clients.get(&lang).expect("Client was initialized");
        Ok(Server { client })
    }
}

#[derive(Debug)]
struct Project {
    path: PathBuf,
    folder: WorkspaceFolder,
}

impl Project {
    fn new(path: &Path) -> Result<Self> {
        let path = fs::canonicalize(path)?;
        if !path.is_dir() {
            return Err(Error::InvalidProjectDirectory(path));
        }

        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let folder = WorkspaceFolder {
            uri: file_uri(&path)?,
            name,
        };

        Ok(Self { path, folder })
    }
}

pub struct Server<'a> {
    client: &'a Client,
}

impl Server<'_> {
    pub fn get_semantic_tokens(
        &self,
        input: &str,
        path: Option<&Path>,
    ) -> Result<Option<(Vec<SemanticToken>, SemanticTokensLegend)>> {
        self.client.get_semantic_tokens(input, path)
    }
}

#[derive(Debug)]
struct Document {
    uri: Uri,
    temporary_document: Option<TemporaryDocument>,
}

impl Document {
    fn new(text: &str, path: Option<&Path>, language: &str) -> Result<Document> {
        let (path, temporary_path) = match path {
            Some(path) => (fs::canonicalize(path)?, None),
            None => {
                let temporary_document = create_temporary_document(text, language)?;
                (temporary_document.path.clone(), Some(temporary_document))
            }
        };

        Ok(Document {
            uri: file_uri(&path)?,
            temporary_document: temporary_path,
        })
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        drop(self.temporary_document.take());
    }
}

#[derive(Debug)]
struct TemporaryDocument {
    path: PathBuf,
}

impl Drop for TemporaryDocument {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
struct Connection {
    child: Child,
    rpc: rpc::Connection,
}

/// A synchronous LSP client connected to one language server over stdio.
#[derive(Debug)]
struct Client {
    connection: Mutex<Connection>,
    language: String,
    semantic_tokens_legend: Option<SemanticTokensLegend>,
}

impl Client {
    fn new(
        command_entry: &CommandEntry,
        language: &str,
        project: Option<&Project>,
        log: LogLevel,
    ) -> Result<Client> {
        let mut connection = Client::spawn_server(command_entry, project, log)?;
        let semantic_tokens_legend = match Client::initialize(&mut connection.rpc, project) {
            Ok(legend) => legend,
            Err(error) => {
                let _ = connection.child.kill();
                let _ = connection.child.wait();
                return Err(error);
            }
        };

        Ok(Client {
            connection: Mutex::new(connection),
            language: language.to_string(),
            semantic_tokens_legend,
        })
    }

    fn spawn_server(
        command_entry: &CommandEntry,
        project: Option<&Project>,
        log: LogLevel,
    ) -> Result<Connection> {
        let mut command = server_command(command_entry, project);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| Error::FailedServerCommand(command_entry.command.clone(), error))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("Server stdin was not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("Server stdout was not piped"))?;

        Ok(Connection {
            child,
            rpc: rpc::Connection::new(stdout, stdin, log),
        })
    }

    fn initialize(
        connection: &mut rpc::Connection,
        project: Option<&Project>,
    ) -> Result<Option<SemanticTokensLegend>> {
        let result = connection.request::<Initialize>(initialize_params(project))?;
        connection.notify::<Initialized>(InitializedParams {})?;

        Ok(result
            .capabilities
            .semantic_tokens_provider
            .and_then(full_semantic_tokens_legend))
    }

    fn get_semantic_tokens(
        &self,
        text: &str,
        path: Option<&Path>,
    ) -> Result<Option<(Vec<SemanticToken>, SemanticTokensLegend)>> {
        let Some(legend) = self.semantic_tokens_legend.clone() else {
            return Ok(None);
        };

        let document = Document::new(text, path, &self.language)?;
        let uri = document.uri.clone();
        let mut connection = self.connection.lock().expect("Connection lock poisoned");

        connection
            .rpc
            .notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                text_document: TextDocumentItem::new(
                    uri.clone(),
                    self.language.clone(),
                    1,
                    text.to_string(),
                ),
            })?;
        connection
            .rpc
            .wait_for_progress(DOCUMENT_OPEN_WAIT_TIMEOUT, PROGRESS_END_WAIT_TIMEOUT)?;

        let token_result = connection
            .rpc
            .request::<SemanticTokensFullRequest>(SemanticTokensParams {
                text_document: TextDocumentIdentifier::new(uri.clone()),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .map(|response| {
                response.map(|result| match result {
                    SemanticTokensResult::Tokens(tokens) => tokens.data,
                    SemanticTokensResult::Partial(tokens) => tokens.data,
                })
            });

        let close_result =
            connection
                .rpc
                .notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier::new(uri),
                });

        match (token_result, close_result) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error.into()),
            (Ok(tokens), Ok(())) => Ok(tokens.map(|tokens| (tokens, legend))),
        }
    }
}

fn initialize_params(project: Option<&Project>) -> InitializeParams {
    let semantic_tokens = SemanticTokensClientCapabilities {
        requests: SemanticTokensClientCapabilitiesRequests {
            range: Some(false),
            full: Some(SemanticTokensFullOptions::Bool(true)),
        },
        token_types: vec![
            SemanticTokenType::NAMESPACE,
            SemanticTokenType::TYPE,
            SemanticTokenType::CLASS,
            SemanticTokenType::ENUM,
            SemanticTokenType::INTERFACE,
            SemanticTokenType::STRUCT,
            SemanticTokenType::TYPE_PARAMETER,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PROPERTY,
            SemanticTokenType::ENUM_MEMBER,
            SemanticTokenType::EVENT,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::METHOD,
            SemanticTokenType::MACRO,
            SemanticTokenType::KEYWORD,
            SemanticTokenType::MODIFIER,
            SemanticTokenType::COMMENT,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::REGEXP,
            SemanticTokenType::OPERATOR,
            SemanticTokenType::DECORATOR,
            SemanticTokenType::new("constant"),
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::DEFINITION,
            SemanticTokenModifier::READONLY,
            SemanticTokenModifier::STATIC,
            SemanticTokenModifier::DEPRECATED,
            SemanticTokenModifier::ABSTRACT,
            SemanticTokenModifier::ASYNC,
            SemanticTokenModifier::MODIFICATION,
            SemanticTokenModifier::DOCUMENTATION,
            SemanticTokenModifier::DEFAULT_LIBRARY,
        ],
        formats: vec![TokenFormat::RELATIVE],
        ..Default::default()
    };

    #[allow(deprecated)]
    InitializeParams {
        process_id: Some(std::process::id()),
        capabilities: ClientCapabilities {
            window: Some(WindowClientCapabilities {
                work_done_progress: Some(true),
                show_message: Some(Default::default()),
                ..Default::default()
            }),
            text_document: Some(TextDocumentClientCapabilities {
                semantic_tokens: Some(semantic_tokens),
                ..Default::default()
            }),
            workspace: project.map(|_| WorkspaceClientCapabilities {
                workspace_folders: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        root_uri: project.map(|project| project.folder.uri.clone()),
        workspace_folders: project.map(|project| vec![project.folder.clone()]),
        ..Default::default()
    }
}

fn server_command(command_entry: &CommandEntry, project: Option<&Project>) -> Command {
    let mut command = Command::new(&command_entry.command);
    command.args(&command_entry.args);
    if let Some(project) = project {
        command.current_dir(&project.path);
    }
    command
}

fn create_temporary_document(text: &str, language: &str) -> Result<TemporaryDocument> {
    let extension = document_extension(language);
    let directory = std::env::temp_dir();

    for _ in 0..MAX_TEMP_FILE_ATTEMPTS {
        let id = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            "{TEMP_FILE_PREFIX}-{}-{id}.{extension}",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let temporary_document = TemporaryDocument { path };
                file.write_all(text.as_bytes())?;
                return Ok(temporary_document);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Could not allocate a unique temporary document path",
    )
    .into())
}

fn document_extension(language: &str) -> &str {
    match language {
        "rust" => "rs",
        "python" => "py",
        "typescript" => "ts",
        "javascript" => "js",
        "cpp" => "cpp",
        "csharp" => "cs",
        "kotlin" => "kt",
        "haskell" => "hs",
        "ocaml" => "ml",
        "latex" => "tex",
        other if other.bytes().all(|byte| byte.is_ascii_alphanumeric()) => other,
        _ => DEFAULT_DOCUMENT_EXTENSION,
    }
}

fn file_uri(path: &Path) -> Result<Uri> {
    debug_assert!(path.is_absolute());
    let mut encoded = String::from(FILE_URI_PREFIX);

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        percent_encode_path(path.as_os_str().as_bytes(), &mut encoded);
    }

    #[cfg(not(unix))]
    {
        encoded.push('/');
        let path = path.to_string_lossy().replace('\\', "/");
        percent_encode_path(path.as_bytes(), &mut encoded);
    }

    Uri::from_str(&encoded).map_err(|_| Error::InvalidDocumentUri(path.to_path_buf()))
}

fn percent_encode_path(path: &[u8], encoded: &mut String) {
    for &byte in path {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(URI_HEX_DIGITS[usize::from(byte >> 4)]));
            encoded.push(char::from(URI_HEX_DIGITS[usize::from(byte & 0x0f)]));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let connection = self
            .connection
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = connection.rpc.request::<Shutdown>(());
        let _ = connection.rpc.notify::<Exit>(());
        let _ = connection.child.wait();
    }
}

fn full_semantic_tokens_legend(
    capability: SemanticTokensServerCapabilities,
) -> Option<SemanticTokensLegend> {
    let options = match capability {
        SemanticTokensServerCapabilities::SemanticTokensOptions(options) => options,
        SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(options) => {
            options.semantic_tokens_options
        }
    };
    match options.full {
        Some(SemanticTokensFullOptions::Bool(true) | SemanticTokensFullOptions::Delta { .. }) => {
            Some(options.legend)
        }
        Some(SemanticTokensFullOptions::Bool(false)) | None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{SemanticTokensOptions, WorkDoneProgressOptions};

    const TEST_ARGUMENT: &str = "--stdio";
    const TEST_COMMAND: &str = "language-server";
    const TEST_SOURCE: &str = "fn main() {}";

    fn test_project() -> Project {
        Project::new(&std::env::temp_dir()).unwrap()
    }

    fn test_command_entry() -> CommandEntry {
        CommandEntry {
            command: TEST_COMMAND.to_owned(),
            args: vec![TEST_ARGUMENT.to_owned()],
        }
    }

    #[test]
    fn semantic_tokens_require_full_document_support() {
        let legend = SemanticTokensLegend {
            token_types: vec![SemanticTokenType::FUNCTION],
            token_modifiers: Vec::new(),
        };
        let capability = |full| {
            SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                work_done_progress_options: WorkDoneProgressOptions::default(),
                legend: legend.clone(),
                range: None,
                full,
            })
        };

        assert!(full_semantic_tokens_legend(capability(None)).is_none());
        assert!(
            full_semantic_tokens_legend(capability(Some(SemanticTokensFullOptions::Bool(false))))
                .is_none()
        );
        assert_eq!(
            full_semantic_tokens_legend(capability(Some(SemanticTokensFullOptions::Bool(true))))
                .unwrap()
                .token_types[0],
            SemanticTokenType::FUNCTION
        );
    }

    #[test]
    fn client_advertises_window_message_and_progress_support() {
        let params = initialize_params(None);
        let window = params.capabilities.window.unwrap();

        assert_eq!(window.work_done_progress, Some(true));
        assert!(window.show_message.is_some());
    }

    #[test]
    fn file_uri_percent_encodes_path_characters() {
        let path = std::env::temp_dir().join("lighter test#?.rs");
        let uri = file_uri(&path).unwrap();

        assert!(uri.as_str().starts_with(FILE_URI_PREFIX));
        assert!(uri.as_str().ends_with("lighter%20test%23%3F.rs"));
    }

    #[test]
    fn temporary_rust_document_is_file_backed_and_removed_on_drop() {
        let document = Document::new(TEST_SOURCE, None, "rust").unwrap();
        let path = document.temporary_document.as_ref().unwrap().path.clone();

        assert!(document.uri.as_str().starts_with(FILE_URI_PREFIX));
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("rs")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), TEST_SOURCE);

        drop(document);
        assert!(!path.exists());
    }

    #[test]
    #[allow(deprecated)]
    fn project_configures_server_process_and_lsp_workspace() {
        let project = test_project();
        let command = server_command(&test_command_entry(), Some(&project));
        let params = initialize_params(Some(&project));
        let workspace = params.capabilities.workspace.as_ref().unwrap();
        let folders = params.workspace_folders.as_ref().unwrap();

        assert_eq!(command.get_program(), TEST_COMMAND);
        assert_eq!(command.get_args().collect::<Vec<_>>(), [TEST_ARGUMENT]);
        assert_eq!(command.get_current_dir(), Some(project.path.as_path()));
        assert_eq!(params.root_uri.as_ref(), Some(&project.folder.uri));
        assert_eq!(folders, std::slice::from_ref(&project.folder));
        assert_eq!(workspace.workspace_folders, Some(true));
    }

    #[test]
    #[allow(deprecated)]
    fn server_has_no_workspace_without_project() {
        let command = server_command(&test_command_entry(), None);
        let params = initialize_params(None);

        assert_eq!(command.get_current_dir(), None);
        assert!(params.root_uri.is_none());
        assert!(params.workspace_folders.is_none());
        assert!(params.capabilities.workspace.is_none());
    }
}
