mod rpc;

use arborium::advanced::Span;
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
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use thiserror::Error;

use crate::LangName;
use crate::logging::LogLevel;

const FILE_URI_PREFIX: &str = "file://";
const TEMP_FILE_PREFIX: &str = "lighter";
const DEFAULT_DOCUMENT_EXTENSION: &str = "txt";
const MAX_TEMP_FILE_ATTEMPTS: usize = 100;
const URI_HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";
const DOCUMENT_OPEN_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const PROGRESS_END_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const TYPE_TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::CLASS,
    SemanticTokenType::INTERFACE,
    SemanticTokenType::STRUCT,
];
const TOKEN_CAPTURE_MAPPINGS: &[(SemanticTokenType, &str)] = &[
    (SemanticTokenType::TYPE_PARAMETER, "type.parameter"),
    (SemanticTokenType::PARAMETER, "variable.parameter"),
    (SemanticTokenType::ENUM, "type.enum"),
    (SemanticTokenType::ENUM_MEMBER, "type.enum.variant"),
    (SemanticTokenType::MODIFIER, "keyword.modifier"),
    (SemanticTokenType::REGEXP, "string.regexp"),
];

static NEXT_TEMP_FILE_ID: AtomicUsize = AtomicUsize::new(0);

#[derive(Error, Debug)]
pub enum Error {
    #[error("No language server available for {0}")]
    NoServer(LangName),
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
    #[error("Language server for '{0}' does not provide semantic tokens")]
    NoSemanticTokens(LangName),
}

pub type CaptureMapping = HashMap<String, String>;
pub type LangCaptureMapping = HashMap<LangName, CaptureMapping>;
pub type Commands = HashMap<LangName, CommandEntry>;
pub type Result<T> = std::result::Result<T, Error>;

/// A single LSP server entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEntry {
    /// The executable name or path (e.g. "rust-analyzer").
    pub command: String,
    /// Arguments passed to the server (e.g. `["--stdio"]`).
    pub args: Vec<String>,
}

impl CommandEntry {
    pub fn new<S: AsRef<str>>(command: &str, args: &[S]) -> Self {
        Self {
            command: command.to_owned(),
            args: args.iter().map(|arg| arg.as_ref().to_owned()).collect(),
        }
    }
}

/// Default server commands
pub fn default_commands() -> Commands {
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
        .map(|(language, command, args)| {
            (LangName::from(*language), CommandEntry::new(command, args))
        })
        .collect()
}

#[derive(Debug)]
pub struct ServerRegistry {
    clients: HashMap<LangName, Client>,
    commands: Commands,
    general_mapping: CaptureMapping,
    lang_mapping: LangCaptureMapping,
    project: Option<Project>,
    log: LogLevel,
}

impl Default for ServerRegistry {
    fn default() -> Self {
        Self {
            commands: default_commands(),
            clients: HashMap::new(),
            general_mapping: HashMap::new(),
            lang_mapping: HashMap::new(),
            project: None,
            log: LogLevel::default(),
        }
    }
}

impl ServerRegistry {
    pub fn new(
        commands: Commands,
        general_mapping: CaptureMapping,
        lang_mapping: LangCaptureMapping,
        project: Option<&Path>,
        log: LogLevel,
    ) -> Result<Self> {
        Ok(Self {
            commands,
            general_mapping,
            lang_mapping,
            project: project.map(Project::new).transpose()?,
            log,
            clients: HashMap::new(),
        })
    }

    pub fn get_server(&mut self, lang: LangName) -> Result<Server<'_>> {
        let client = match self.clients.entry(lang.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let command = self
                    .commands
                    .get(&lang)
                    .ok_or_else(|| Error::NoServer(lang.clone()))?;
                entry.insert(Client::new(
                    command,
                    &lang,
                    self.project.as_ref(),
                    self.log,
                )?)
            }
        };
        let mapping = self.lang_mapping.entry(lang).or_default();

        mapping.extend(
            self.general_mapping
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        Ok(Server { client, mapping })
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
    mapping: &'a HashMap<String, String>,
}

impl Server<'_> {
    pub fn get_semantic_spans(
        &self,
        input: &str,
        path: Option<&Path>,
        pattern_index: u32,
    ) -> Result<Vec<Span>> {
        let tokens = self.client.get_semantic_tokens(input, path)?;
        Ok(semantic_tokens_to_spans(
            input,
            &tokens,
            &self.client.semantic_tokens_legend,
            pattern_index,
            self.mapping,
        ))
    }
}

#[derive(Debug)]
struct Document {
    uri: Uri,
    _temporary: Option<TemporaryDocument>,
}

impl Document {
    fn new(text: &str, path: Option<&Path>, language: &str) -> Result<Self> {
        let (path, temporary) = match path {
            Some(path) => (fs::canonicalize(path)?, None),
            None => {
                let temporary_document = create_temporary_document(text, language)?;
                (temporary_document.path.clone(), Some(temporary_document))
            }
        };

        Ok(Self {
            uri: file_uri(&path)?,
            _temporary: temporary,
        })
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
    language: LangName,
    semantic_tokens_legend: SemanticTokensLegend,
}

impl Client {
    fn new(
        command_entry: &CommandEntry,
        language: &LangName,
        project: Option<&Project>,
        log: LogLevel,
    ) -> Result<Self> {
        let mut connection = Self::spawn_server(command_entry, project, log)?;
        let semantic_tokens_legend = match Self::initialize(&mut connection.rpc, project) {
            Ok(None) => Err(Error::NoSemanticTokens(language.clone())),
            Ok(Some(legend)) => Ok(legend),
            Err(error) => {
                let _ = connection.child.kill();
                let _ = connection.child.wait();
                return Err(error);
            }
        }?;

        Ok(Self {
            connection: Mutex::new(connection),
            language: language.clone(),
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

    fn get_semantic_tokens(&self, text: &str, path: Option<&Path>) -> Result<Vec<SemanticToken>> {
        let document = Document::new(text, path, &self.language)?;
        let uri = document.uri.clone();
        let mut connection = self.connection.lock().expect("Connection lock poisoned");

        connection
            .rpc
            .notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                text_document: TextDocumentItem::new(
                    uri.clone(),
                    self.language.to_string(),
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
                // TODO: should we act on a None result?
                response.map_or(Vec::new(), |result| match result {
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

        let tokens = token_result?;
        close_result?;
        Ok(tokens)
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
        other if !other.is_empty() && other.bytes().all(|byte| byte.is_ascii_alphanumeric()) => {
            other
        }
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

fn semantic_tokens_to_spans(
    source: &str,
    tokens: &[SemanticToken],
    legend: &SemanticTokensLegend,
    pattern_index: u32,
    captures: &HashMap<String, String>,
) -> Vec<Span> {
    let lines = LineIndex::new(source);
    let mut line = 0_u32;
    let mut character = 0_u32;

    tokens
        .iter()
        .filter_map(|token| {
            line = line.checked_add(token.delta_line)?;
            character = if token.delta_line == 0 {
                character.checked_add(token.delta_start)?
            } else {
                token.delta_start
            };

            let token_type = legend.token_types.get(token.token_type as usize)?;
            let (start, end) = lines.byte_range(line, character, token.length)?;

            Some(Span {
                start: u32::try_from(start).ok()?,
                end: u32::try_from(end).ok()?,
                capture: capture_for_token_type(token_type, captures).to_owned(),
                pattern_index,
            })
        })
        .collect()
}

fn capture_for_token_type<'a>(
    token_type: &'a SemanticTokenType,
    captures: &'a HashMap<String, String>,
) -> &'a str {
    captures
        .get(token_type.as_str())
        .map(|capture| capture.as_str())
        .unwrap_or_else(|| default_capture_for_token_type(token_type))
}

fn default_capture_for_token_type(token_type: &SemanticTokenType) -> &str {
    if TYPE_TOKEN_TYPES.contains(token_type) {
        return "type";
    }

    if let Some(capture) = TOKEN_CAPTURE_MAPPINGS
        .iter()
        .find_map(|(candidate, capture)| (candidate == token_type).then_some(*capture))
    {
        return capture;
    }

    // NOTE: currently unsupported default LSP token types are `event` and `decorator`.
    token_type.as_str()
}

struct LineIndex<'source> {
    source: &'source str,
    starts: Vec<usize>,
}

impl<'source> LineIndex<'source> {
    fn new(source: &'source str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        Self { source, starts }
    }

    fn byte_range(&self, line: u32, character: u32, length: u32) -> Option<(usize, usize)> {
        let line = usize::try_from(line).ok()?;
        let start = *self.starts.get(line)?;
        let mut end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source.len());

        if self.source.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\n') {
            end -= 1;
        }
        if self.source.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r') {
            end -= 1;
        }

        let line_source = self.source.get(start..end)?;
        let token_end = character.checked_add(length)?;
        let relative_start = utf16_column_to_byte(line_source, character)?;
        let relative_end = utf16_column_to_byte(line_source, token_end)?;

        (relative_start < relative_end).then(|| (start + relative_start, start + relative_end))
    }
}

fn utf16_column_to_byte(line: &str, column: u32) -> Option<usize> {
    let mut utf16_column = 0_u32;

    for (byte, character) in line.char_indices() {
        if utf16_column == column {
            return Some(byte);
        }

        utf16_column = utf16_column.checked_add(character.len_utf16() as u32)?;
        if utf16_column > column {
            return None;
        }
    }

    (utf16_column == column).then_some(line.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        SemanticTokensOptions, SemanticTokensRegistrationOptions, StaticRegistrationOptions,
        TextDocumentRegistrationOptions, WorkDoneProgressOptions,
    };

    const TEST_ARGUMENT: &str = "--stdio";
    const TEST_COMMAND: &str = "language-server";
    const TEST_SOURCE: &str = "fn main() {}";

    fn test_project() -> Project {
        Project::new(&std::env::temp_dir()).unwrap()
    }

    fn test_command_entry() -> CommandEntry {
        CommandEntry::new(TEST_COMMAND, &[TEST_ARGUMENT])
    }

    fn semantic_options(
        legend: &SemanticTokensLegend,
        full: Option<SemanticTokensFullOptions>,
    ) -> SemanticTokensOptions {
        SemanticTokensOptions {
            work_done_progress_options: WorkDoneProgressOptions::default(),
            legend: legend.clone(),
            range: None,
            full,
        }
    }

    #[test]
    fn semantic_tokens_require_full_document_support() {
        let legend = SemanticTokensLegend {
            token_types: vec![SemanticTokenType::FUNCTION],
            token_modifiers: Vec::new(),
        };
        let capability = |full| semantic_options(&legend, full).into();

        for unsupported in [None, Some(SemanticTokensFullOptions::Bool(false))] {
            assert!(full_semantic_tokens_legend(capability(unsupported)).is_none());
        }

        let registration = SemanticTokensRegistrationOptions {
            text_document_registration_options: TextDocumentRegistrationOptions::default(),
            semantic_tokens_options: semantic_options(
                &legend,
                Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
            ),
            static_registration_options: StaticRegistrationOptions::default(),
        };
        for supported in [
            capability(Some(SemanticTokensFullOptions::Bool(true))),
            SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(registration),
        ] {
            assert_eq!(full_semantic_tokens_legend(supported), Some(legend.clone()));
        }
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
    fn document_extensions_are_safe_and_language_appropriate() {
        let cases = [
            ("rust", "rs"),
            ("typescript", "ts"),
            ("csharp", "cs"),
            ("gleam", "gleam"),
            ("../rust", DEFAULT_DOCUMENT_EXTENSION),
            ("", DEFAULT_DOCUMENT_EXTENSION),
        ];

        for (language, expected) in cases {
            assert_eq!(document_extension(language), expected);
        }
    }

    #[test]
    fn temporary_rust_document_is_file_backed_and_removed_on_drop() {
        let document = Document::new(TEST_SOURCE, None, "rust").unwrap();
        let path = document._temporary.as_ref().unwrap().path.clone();
        let other_document = Document::new(TEST_SOURCE, None, "rust").unwrap();
        let other_path = other_document._temporary.as_ref().unwrap().path.clone();

        assert!(document.uri.as_str().starts_with(FILE_URI_PREFIX));
        assert_ne!(path, other_path);
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("rs")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), TEST_SOURCE);

        drop(document);
        assert!(!path.exists());
        assert!(other_path.exists());
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

    #[test]
    fn maps_lsp_token_types_to_supported_capture_names() {
        let cases = [
            (SemanticTokenType::CLASS, "type"),
            (SemanticTokenType::INTERFACE, "type"),
            (SemanticTokenType::STRUCT, "type"),
            (SemanticTokenType::TYPE_PARAMETER, "type.parameter"),
            (SemanticTokenType::PARAMETER, "variable.parameter"),
            (SemanticTokenType::ENUM, "type.enum"),
            (SemanticTokenType::ENUM_MEMBER, "type.enum.variant"),
            (SemanticTokenType::MODIFIER, "keyword.modifier"),
            (SemanticTokenType::REGEXP, "string.regexp"),
            (SemanticTokenType::EVENT, "event"),
        ];

        for (token_type, expected) in cases {
            assert_eq!(default_capture_for_token_type(&token_type), expected);
        }
    }
}
