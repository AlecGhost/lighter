use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use clap::Parser;
use clap::builder::PossibleValuesParser;

mod lighter;
mod logging;
mod lsp;

#[derive(Parser, Debug)]
#[command(name = "lighter", version, about)]
struct Cli {
    /// Source file to highlight (reads stdin when omitted).
    file: Option<PathBuf>,

    /// Project directory exposed to the language server as its workspace.
    #[arg(long, value_name = "DIR")]
    project: Option<PathBuf>,

    /// Language name (e.g. "rust", "python").
    /// Inferred from file extension if a file path is provided.
    #[arg(short, long)]
    lang: Option<String>,

    /// Path to a TOML config file
    /// that maps language names to LSP server commands.
    /// Falls back to built-in defaults when omitted.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Name of a built-in Arborium theme.
    #[arg(
        long,
        conflicts_with = "custom_theme",
        ignore_case = true,
        value_parser = builtin_theme_parser()
    )]
    theme: Option<String>,

    /// Path to a custom TOML theme file.
    #[arg(long, conflicts_with = "theme")]
    custom_theme: Option<PathBuf>,

    /// Output highlighted ANSI or HTML.
    #[arg(short, long, value_enum, default_value_t = lighter::Output::Ansi)]
    format: lighter::Output,

    /// Disable LSP semantic highlighting.
    #[arg(long)]
    no_lsp: bool,

    /// Disable tree-sitter syntax highlighting.
    #[arg(long)]
    no_tree_sitter: bool,

    /// Set stderr logging verbosity.
    #[arg(
        long,
        value_enum,
        ignore_case = true,
        default_value_t = logging::LogLevel::Error
    )]
    log: logging::LogLevel,
}

#[derive(serde::Deserialize)]
struct ConfigRaw {
    #[serde(default)]
    commands: HashMap<String, String>,
    #[serde(default)]
    captures: lighter::CaptureMappings,
}

struct Config {
    commands: HashMap<lsp::LangName, lsp::CommandEntry>,
    captures: lighter::CaptureMappings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            commands: lsp::default_commands(),
            captures: lighter::CaptureMappings::new(),
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
///
/// [captures]
/// param = "parameter"
///
/// [captures.rust]
/// const = "constant"
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
    Ok(Config {
        commands,
        captures: config.captures,
    })
}

fn load_custom_theme(path: &PathBuf) -> Result<arborium::theme::Theme> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("Failed to read theme file '{}'", path.display()))?;
    arborium::theme::Theme::from_toml(&text)
        .with_context(|| format!("Invalid theme file '{}'", path.display()))
}

fn builtin_theme_parser() -> PossibleValuesParser {
    PossibleValuesParser::new(
        arborium::theme::builtin::all()
            .into_iter()
            .map(|theme| theme.name),
    )
}

fn load_builtin_theme(name: &str) -> Result<arborium::theme::Theme> {
    arborium::theme::builtin::all()
        .into_iter()
        .find(|theme| theme.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow!("Unknown built-in theme '{name}'"))
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
    let config = match (cli.no_lsp, cli.config.as_ref()) {
        (true, _) => Config {
            commands: HashMap::new(),
            captures: lighter::CaptureMappings::new(),
        },
        (false, Some(path)) => load_config(path)?,
        (false, None) => Config::default(),
    };
    let theme = match (cli.theme.as_deref(), cli.custom_theme.as_ref()) {
        (Some(name), None) => load_builtin_theme(name)?,
        (None, Some(path)) => load_custom_theme(path)?,
        (None, None) => lighter::default_theme(),
        (Some(_), Some(_)) => unreachable!("clap rejects conflicting theme options"),
    };

    let mut registry = lsp::ServerRegistry::new(config.commands, cli.project.as_deref(), cli.log)?;
    let output = lighter::highlight(
        &source,
        cli.file.as_deref(),
        lang,
        &mut registry,
        &lighter::HighlightOptions {
            output: cli.format,
            lsp: !cli.no_lsp,
            tree_sitter: !cli.no_tree_sitter,
            theme,
            captures: config.captures,
            log: cli.log,
        },
    )?;

    print!("{output}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    const BUILTIN_THEME_NAME: &str = "Dracula";
    const CLI_NAME: &str = "lighter";
    const LSP_CAPTURE_NAME: &str = "const";
    const HIGHLIGHT_CAPTURE_NAME: &str = "constant";
    const LANGUAGE_NAME: &str = "rust";
    const OTHER_LANGUAGE_NAME: &str = "python";
    const LANGUAGE_CAPTURE_NAME: &str = "constant.rust";
    const CAPTURE_CONFIG: &str = r#"
[captures]
const = "constant"

[captures.rust]
const = "constant.rust"
"#;
    const PROJECT_ARGUMENT: &str = "--project";
    const PROJECT_DIRECTORY: &str = ".";
    const LOG_ARGUMENT: &str = "--log";
    const DEBUG_LOG_LEVEL: &str = "DEBUG";
    const REMOVED_DEBUG_ARGUMENT: &str = "--debug";
    const DEBUG_FORMAT: &str = "debug";
    const FORMAT_ARGUMENT: &str = "--format";

    fn parse_builtin_theme(name: &str) -> clap::error::Result<Cli> {
        Cli::try_parse_from([CLI_NAME, "--theme", name])
    }

    #[test]
    fn dynamic_parser_accepts_builtin_theme_case_insensitively() {
        let cli = parse_builtin_theme(&BUILTIN_THEME_NAME.to_lowercase()).unwrap();
        let theme = load_builtin_theme(cli.theme.as_deref().unwrap()).unwrap();

        assert_eq!(theme.name, BUILTIN_THEME_NAME);
    }

    #[test]
    fn dynamic_parser_rejects_unknown_theme() {
        let error = parse_builtin_theme("unknown").unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn parses_project_directory() {
        let cli = Cli::try_parse_from([CLI_NAME, PROJECT_ARGUMENT, PROJECT_DIRECTORY]).unwrap();

        assert_eq!(cli.project, Some(PathBuf::from(PROJECT_DIRECTORY)));
    }

    #[test]
    fn log_level_defaults_to_error_and_accepts_uppercase() {
        let default_cli = Cli::try_parse_from([CLI_NAME]).unwrap();
        let debug_cli = Cli::try_parse_from([CLI_NAME, LOG_ARGUMENT, DEBUG_LOG_LEVEL]).unwrap();

        assert_eq!(default_cli.log, logging::LogLevel::Error);
        assert_eq!(debug_cli.log, logging::LogLevel::Debug);
    }

    #[test]
    fn debug_is_not_an_output_format() {
        let error = Cli::try_parse_from([CLI_NAME, FORMAT_ARGUMENT, DEBUG_FORMAT]).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn debug_flag_is_removed() {
        let error = Cli::try_parse_from([CLI_NAME, REMOVED_DEBUG_ARGUMENT]).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn parses_capture_mappings_without_commands() {
        let config: ConfigRaw = toml::from_str(CAPTURE_CONFIG).unwrap();

        assert!(config.commands.is_empty());
        assert_eq!(
            config.captures.get(LSP_CAPTURE_NAME, OTHER_LANGUAGE_NAME),
            Some(HIGHLIGHT_CAPTURE_NAME)
        );
        assert_eq!(
            config.captures.get(LSP_CAPTURE_NAME, LANGUAGE_NAME),
            Some(LANGUAGE_CAPTURE_NAME)
        );
    }
}
