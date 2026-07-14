mod rpc;

use lsp_types::notification::{DidCloseTextDocument, DidOpenTextDocument, Exit, Initialized};
use lsp_types::request::{Initialize, SemanticTokensFullRequest, Shutdown};
use lsp_types::{
    ClientCapabilities, DidCloseTextDocumentParams, DidOpenTextDocumentParams, InitializeParams,
    InitializedParams, PartialResultParams, SemanticToken, SemanticTokenModifier,
    SemanticTokenType, SemanticTokensClientCapabilities, SemanticTokensClientCapabilitiesRequests,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, TextDocumentClientCapabilities, TextDocumentIdentifier,
    TextDocumentItem, TokenFormat, Uri, WorkDoneProgressParams,
};
use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use thiserror::Error;

pub type LangName = Rc<str>;

const VIRTUAL_DOCUMENT_URI_PREFIX: &str = "untitled:";

#[derive(Error, Debug)]
pub enum Error {
    #[error("No language server available for {0}")]
    NoServer(String),
    #[error("Failed to start server for {0}: {1}")]
    FailedServerCommand(String, #[source] std::io::Error),
    #[error("Language server I/O failed: {0}")]
    Io(#[from] std::io::Error),
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
        ("python", "pylsp", &[]),
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
}

impl ServerRegistry {
    pub fn new(commands: HashMap<LangName, CommandEntry>) -> ServerRegistry {
        ServerRegistry {
            commands,
            ..Default::default()
        }
    }
}

impl<'a> ServerRegistry {
    pub fn get_server(&'a mut self, lang: LangName) -> Result<Server<'a>> {
        if !self.clients.contains_key(&lang) {
            if let Some(command_entry) = self.commands.get(&lang) {
                let client = Client::new(command_entry, &lang)?;
                self.clients.insert(lang.clone(), client);
            } else {
                return Err(Error::NoServer(lang.to_string()));
            }
        }
        let client = self.clients.get(&lang).expect("Client was initialized");
        Ok(Server { client })
    }
}

pub struct Server<'a> {
    client: &'a Client,
}

impl Server<'_> {
    pub fn get_semantic_tokens(
        &self,
        input: &str,
    ) -> Result<Option<(Vec<SemanticToken>, SemanticTokensLegend)>> {
        self.client.get_semantic_tokens(input)
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
    next_file_id: AtomicUsize,
}

impl Client {
    fn new(command_entry: &CommandEntry, language: &str) -> Result<Client> {
        let mut connection = Client::spawn_server(command_entry)?;
        let semantic_tokens_legend = match Client::initialize(&mut connection.rpc) {
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
            next_file_id: AtomicUsize::new(0),
        })
    }

    fn spawn_server(command_entry: &CommandEntry) -> Result<Connection> {
        let mut child = Command::new(&command_entry.command)
            .args(&command_entry.args)
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
            rpc: rpc::Connection::new(stdout, stdin),
        })
    }

    fn initialize(connection: &mut rpc::Connection) -> Result<Option<SemanticTokensLegend>> {
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
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            capabilities: ClientCapabilities {
                text_document: Some(TextDocumentClientCapabilities {
                    semantic_tokens: Some(semantic_tokens),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let result = connection.request::<Initialize>(params)?;
        connection.notify::<Initialized>(InitializedParams {})?;

        Ok(result
            .capabilities
            .semantic_tokens_provider
            .and_then(full_semantic_tokens_legend))
    }

    fn get_semantic_tokens(
        &self,
        text: &str,
    ) -> Result<Option<(Vec<SemanticToken>, SemanticTokensLegend)>> {
        let Some(legend) = self.semantic_tokens_legend.clone() else {
            return Ok(None);
        };

        let file_id = self.next_file_id.fetch_add(1, Ordering::Relaxed);
        let uri = Uri::from_str(&format!("{VIRTUAL_DOCUMENT_URI_PREFIX}{file_id}"))
            .expect("Generated virtual document URI is valid");
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
}
