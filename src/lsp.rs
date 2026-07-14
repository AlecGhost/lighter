use lsp_types::{SemanticToken, SemanticTokensLegend};
use std::collections::HashMap;
use std::os::unix::io::OwnedFd;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Mutex;
use thiserror::Error;

pub type LangName = Rc<str>;
type DocId = usize;

#[derive(Error, Debug)]
pub enum Error {
    #[error("No language server available for {0}")]
    NoServer(String),
    #[error("Failed to start server for {0}:\n\n{1}")]
    FailedServerCommand(String, std::io::Error),
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
        // Initialize client if not initialized
        if !self.clients.contains_key(&lang) {
            if let Some(command_entry) = self.commands.get(&lang) {
                let client = Client::new(command_entry)?;
                self.clients.insert(lang.clone(), client);
            } else {
                return Err(Error::NoServer(lang.to_string()));
            }
        }
        // Client is already initialized
        let client = self.clients.get(&lang).expect("Client was initialized");
        Ok(Server { client })
    }
}

pub struct Server<'a> {
    client: &'a Client,
}

impl Server<'_> {
    pub fn get_semantic_tokens(&self, input: &str) -> Vec<Token> {
        let doc_id = self.client.open_doc(input);
        let tokens = self.client.get_semantic_tokens(doc_id);
        self.client.close_doc(doc_id);
        tokens
    }
}

pub struct Token {
    pub token_type: &'static str,
    pub token_modifiers: Vec<&'static str>,
}

/// LSP client that communicates with language servers via stdin/stdout file descriptors.
#[derive(Debug)]
struct Client {
    /// Write requests/notifications here (the server's stdin).
    stdin: OwnedFd,
    /// Read responses/notifications from here (the server's stdout).
    stdout: OwnedFd,
    next_file_id: Mutex<usize>,
}

impl Client {
    fn new(command_entry: &CommandEntry) -> Result<Client> {
        let (stdin, stdout) = Client::spawn_server(command_entry)?;
        let client = Client {
            stdin,
            stdout,
            next_file_id: Mutex::new(0),
        };
        client.initialize();
        Ok(client)
    }

    fn spawn_server(command_entry: &CommandEntry) -> Result<(OwnedFd, OwnedFd)> {
        let child = Command::new(&command_entry.command)
            .args(&command_entry.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::FailedServerCommand(command_entry.command.clone(), e))?;

        let child_stdin = child.stdin.expect("stdin handle present");
        let child_stdout = child.stdout.expect("stdout handle present");

        let stdin: OwnedFd = child_stdin.into();
        let stdout: OwnedFd = child_stdout.into();

        Ok((stdin, stdout))
    }

    /// This function must be called after connecting to the server.
    fn initialize(&self) {
        // TODO: send initialize request and initialized notification
        todo!()
    }

    /// This function must be called before dropping the client connection.
    fn shutdown(&self) {
        // TODO: send shutdown request and exit notification
        todo!()
    }

    fn open_doc(&self, text: &str) -> DocId {
        // TODO: impl
        todo!()
    }

    fn close_doc(&self, doc_id: DocId) {
        // TODO: impl
    }

    fn get_semantic_tokens(&self, doc_id: DocId) -> Vec<Token> {
        // TODO: impl
        todo!()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shutdown();
    }
}
