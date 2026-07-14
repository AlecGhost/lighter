use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use clap::Parser;

mod lighter;
mod lsp;

#[derive(Parser, Debug)]
#[command(name = "lighter", version, about)]
struct Cli {
    /// Source file to highlight (reads stdin when omitted).
    file: Option<PathBuf>,

    /// Language name (e.g. "rust", "python").
    /// Inferred from file extension if a file path is provided.
    #[arg(short, long)]
    lang: Option<String>,

    /// Path to a TOML config file
    /// that maps language names to LSP server commands.
    /// Falls back to built-in defaults when omitted.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Possible outputs are ANSI-colored terminal output or HTML.
    #[arg(short, long, value_enum, default_value_t = lighter::Output::ANSI)]
    format: lighter::Output,
}

#[derive(serde::Deserialize)]
struct ConfigRaw {
    commands: HashMap<String, String>,
}

struct Config {
    commands: HashMap<lsp::LangName, lsp::CommandEntry>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            commands: lsp::default_commands(),
        }
    }
}

/// Load and parse TOML config file.
///
/// Expected format:
/// ```toml
/// [commands]
/// rust = "rust-analyzer"
/// typescript = "typescript-language-server --stdio"
/// ```
///
/// Configured commands update the default commands.
fn load_config(path: &PathBuf) -> Result<Config> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file '{}'", path.display()))?;
    let config: ConfigRaw = toml::from_str(&text)
        .with_context(|| format!("Invalid toml in config file '{}'", path.display()))?;

    let mut commands = lsp::default_commands();
    for (lang, cmd_str) in config.commands {
        let parts = shlex::split(&cmd_str)
            .with_context(|| format!("Invalid command string for language '{lang}': {cmd_str}"))?;
        if parts.is_empty() {
            bail!("Empty command string for language '{lang}'");
        }
        commands.insert(
            lsp::LangName::from(lang.as_str()),
            lsp::CommandEntry {
                command: parts[0].clone(),
                args: parts[1..].to_vec(),
            },
        );
    }
    Ok(Config { commands })
}

/// Read input source code: from a file path or from stdin.
fn read_input(path: Option<&PathBuf>) -> Result<String> {
    match path {
        Some(p) => {
            let source = fs::read_to_string(p)
                .with_context(|| anyhow!("Failed to read source file '{}'", p.display()))?;
            Ok(source)
        }
        None => {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .context("Failed to read stdin")?;
            Ok(buf)
        }
    }
}

/// Detect language from file extension using arborium.
fn resolve_language(
    lang_option: Option<&str>,
    file_option: Option<&PathBuf>,
) -> Result<lsp::LangName> {
    match (lang_option, file_option) {
        (Some(lang), _) => Ok(lsp::LangName::from(lang)),
        (None, Some(file)) => {
            let path = file
                .to_str()
                .ok_or_else(|| anyhow!("Invalid path {}", file.display()))?;
            arborium::detect_language(path).map(lsp::LangName::from).ok_or_else(|| {
                anyhow!(
                    "Could not detect language from file name '{path}'. Specify it with --lang flag."
                )
            })
        }
        (None, None) => {
            bail!("Could not detect language. Specify it with --lang flag.")
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let source = read_input(cli.file.as_ref())?;
    let lang = resolve_language(cli.lang.as_deref(), cli.file.as_ref())?;
    let config = cli
        .config
        .as_ref()
        .map_or_else(|| Ok(Config::default()), load_config)?;

    let mut registry = lsp::ServerRegistry::new(config.commands);
    let output = lighter::highlight(&source, lang, &mut registry, cli.format)?;

    print!("{output}");

    Ok(())
}
