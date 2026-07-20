use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use lighter::logging;
use lighter::lsp;

use clap::Parser;

const BIN_NAME: &str = "lighter";

#[derive(Parser, Debug)]
#[command(name = BIN_NAME, version, about)]
struct CliInterface {
    /// Source file to highlight (reads stdin when omitted).
    file: Option<PathBuf>,

    /// Project directory exposed to the language server as its workspace.
    #[arg(short, long, value_name = "DIR")]
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

fn builtin_theme_parser() -> clap::builder::PossibleValuesParser {
    clap::builder::PossibleValuesParser::new(
        arborium::theme::builtin::all()
            .into_iter()
            .map(|theme| theme.name),
    )
}

/// Raw config maps directly to how the TOML is structured
#[derive(serde::Deserialize)]
struct RawConfig {
    /// Mapping from language to server spawn command.
    /// ```toml
    /// [servers]
    /// rust = "rust-analyzer --stdio"
    /// ```
    #[serde(default)]
    servers: HashMap<String, String>,
    /// Mapping from LSP captures to Tree-Sitter captures.
    ///
    /// Supports general and language-specific capture mapping.
    /// ```toml
    /// [captures]
    /// decorator = "constant"
    ///
    /// [captures.rust]
    /// const = "constant"
    /// ```
    #[serde(default)]
    captures: HashMap<String, CaptureMapping>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum CaptureMapping {
    Capture(String),
    Language(HashMap<String, String>),
}

/// A `RawConfig` parsed into a format the `lsp::ServerRegistry` understands
struct Config {
    commands: lsp::Commands,
    general_mapping: lsp::CaptureMapping,
    lang_mapping: lsp::LangCaptureMapping,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            commands: lsp::default_commands(),
            general_mapping: lsp::CaptureMapping::new(),
            lang_mapping: lsp::LangCaptureMapping::new(),
        }
    }
}

impl Config {
    /// A config without LSP server commands, thus no LSP can be started
    fn no_lsp() -> Self {
        Self {
            commands: lsp::Commands::new(),
            general_mapping: lsp::CaptureMapping::new(),
            lang_mapping: lsp::LangCaptureMapping::new(),
        }
    }

    /// Read a config from file, parses `RawConfig` and converts it into `Config`
    fn from_file(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file '{}'", path.display()))?;
        let config: RawConfig = toml::from_str(&text)
            .with_context(|| format!("Invalid toml in config file '{}'", path.display()))?;

        // parse commands
        let config_commands = config
            .servers
            .into_iter()
            .map(|(language, command)| {
                let parts = shlex::split(&command).with_context(|| {
                    format!("Invalid command string for language '{language}': {command}")
                })?;
                let Some((program, args)) = parts.split_first() else {
                    bail!("Empty command string for language '{language}'");
                };

                Ok((
                    lighter::LangName::from(language.as_str()),
                    lsp::CommandEntry::new(program, args),
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut commands = lsp::default_commands();
        commands.extend(config_commands);

        let mut general_mapping = HashMap::with_capacity(config.captures.len());
        let mut lang_mapping = HashMap::with_capacity(config.captures.len());
        for (key, val) in config.captures.into_iter() {
            match val {
                CaptureMapping::Capture(mapping) => {
                    general_mapping.insert(key, mapping);
                }
                CaptureMapping::Language(map) => {
                    lang_mapping.insert(lighter::LangName::from(key), map);
                }
            }
        }

        Ok(Config {
            commands,
            general_mapping,
            lang_mapping,
        })
    }

    /// Load a config based on CLI arguments
    fn load(no_lsp: bool, config_path: Option<&Path>) -> Result<Config> {
        match (no_lsp, config_path) {
            (true, _) => Ok(Config::no_lsp()),
            (false, None) => Ok(Config::default()),
            (false, Some(path)) => Config::from_file(path),
        }
    }
}

struct CliOptions {
    file: Option<PathBuf>,
    lang: lighter::LangName,
    theme: arborium::theme::Theme,
    config: Config,
    project: Option<PathBuf>,
    log: logging::LogLevel,
    format: lighter::Output,
    no_tree_sitter: bool,
}

impl TryFrom<CliInterface> for CliOptions {
    type Error = anyhow::Error;

    fn try_from(cli: CliInterface) -> Result<Self> {
        let lang = CliOptions::resolve_language(cli.lang.as_deref(), cli.file.as_deref())?;
        let config = Config::load(cli.no_lsp, cli.config.as_deref())?;
        let theme = CliOptions::load_theme(cli.theme.as_deref(), cli.custom_theme.as_deref())?;
        let CliInterface {
            file,
            project,
            format,
            no_tree_sitter,
            log,
            ..
        } = cli;

        Ok(Self {
            file,
            lang,
            theme,
            config,
            project,
            log,
            format,
            no_tree_sitter,
        })
    }
}

impl CliOptions {
    /// Load theme based on CLI arguments
    fn load_theme(
        builtin_theme: Option<&str>,
        custom_path: Option<&Path>,
    ) -> Result<arborium::theme::Theme> {
        /// Load custom theme from file
        fn load_custom_theme(path: &Path) -> Result<arborium::theme::Theme> {
            let text = fs::read_to_string(path)
                .with_context(|| format!("Failed to read theme file '{}'", path.display()))?;
            arborium::theme::Theme::from_toml(&text)
                .with_context(|| format!("Invalid theme file '{}'", path.display()))
        }

        fn load_builtin_theme(name: &str) -> Result<arborium::theme::Theme> {
            arborium::theme::builtin::all()
                .into_iter()
                .find(|theme| theme.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| anyhow!("Unknown built-in theme '{name}'"))
        }

        match (builtin_theme, custom_path) {
            (Some(name), None) => load_builtin_theme(name),
            (None, Some(path)) => load_custom_theme(path),
            (None, None) => Ok(arborium_theme::builtin::catppuccin_mocha()),
            (Some(_), Some(_)) => unreachable!("clap rejects conflicting theme options"),
        }
    }

    /// Detect language based on CLI arguments.
    ///
    /// Either from language argument or from file extension using arborium.
    fn resolve_language(
        lang_arg: Option<&str>,
        file_arg: Option<&Path>,
    ) -> Result<lighter::LangName> {
        match (lang_arg, file_arg) {
            (Some(lang), _) => Ok(lighter::LangName::from(lang)),
            (None, Some(file_name)) => {
                let path = file_name
                    .to_str()
                    .ok_or_else(|| anyhow!("Invalid path {}", file_name.display()))?;
                arborium::detect_language(path).map(lighter::LangName::from).ok_or_else(|| {
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
}

/// Read input source code: from a file path or from stdin.
fn read_input(path: Option<&Path>) -> Result<String> {
    match path {
        Some(path) => fs::read_to_string(path)
            .with_context(|| anyhow!("Failed to read source file '{}'", path.display())),
        None => {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .context("Failed to read stdin")?;
            Ok(buf)
        }
    }
}

fn main() -> Result<()> {
    let cli = CliInterface::parse();

    let options = CliOptions::try_from(cli)?;
    let source = read_input(options.file.as_deref())?;

    let mut registry = lsp::ServerRegistry::new(
        options.config.commands,
        options.config.general_mapping,
        options.config.lang_mapping,
        options.project.as_deref(),
        options.log,
    )?;
    let output = lighter::highlight(
        &source,
        options.file.as_deref(),
        options.lang,
        &mut registry,
        &lighter::HighlightOptions {
            output: options.format,
            tree_sitter: !options.no_tree_sitter,
            theme: options.theme,
            log: options.log,
        },
    )?;

    print!("{output}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUB_FILE: &str = "stub.py";

    enum CliArgs {
        Project,
        Log,
        Theme,
    }

    impl CliArgs {
        fn as_str(&self) -> &'static str {
            match self {
                CliArgs::Log => "--log",
                CliArgs::Project => "--project",
                CliArgs::Theme => "--theme",
            }
        }
    }

    fn temp_file(suffix: &str, source: &str) -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
        fs::write(&file.path(), source).unwrap();
        file
    }

    fn parse_options(args: &[&str]) -> Result<CliOptions> {
        let cli =
            CliInterface::try_parse_from(std::iter::once(BIN_NAME).chain(args.iter().copied()))?;
        CliOptions::try_from(cli)
    }

    fn builtin_theme() -> arborium::theme::Theme {
        arborium_theme::builtin::dracula()
    }

    fn parse_builtin_theme(name: &str) -> Result<String> {
        parse_options(&[STUB_FILE, CliArgs::Theme.as_str(), &name])
            .map(|options| options.theme.name)
    }

    #[test]
    fn accept_builtin_theme() {
        let theme = builtin_theme();
        let name = parse_builtin_theme(&theme.name).unwrap();
        assert_eq!(theme.name, name);
    }

    #[test]
    fn accept_builtin_theme_case_insensitively() {
        let theme = builtin_theme();
        let name = parse_builtin_theme(&theme.name.to_lowercase()).unwrap();
        assert_eq!(theme.name, name);
    }

    #[test]
    fn reject_unknown_theme() {
        let _theme = builtin_theme();
        let _error = parse_builtin_theme("unknown").unwrap_err();
    }

    #[test]
    fn accept_project_directory() {
        let dir = ".";
        let options = parse_options(&[STUB_FILE, CliArgs::Project.as_str(), dir]).unwrap();

        assert_eq!(options.project, Some(PathBuf::from(dir)));
    }

    #[test]
    fn accept_log_level_case_insensitively() {
        let log_level = logging::LogLevel::Debug;
        let log = log_level.as_str().to_lowercase();
        let options = parse_options(&[STUB_FILE, CliArgs::Log.as_str(), &log]).unwrap();

        assert_eq!(log_level, options.log);
    }

    #[test]
    fn accept_config() {
        let cmd = lsp::CommandEntry {
            command: "custom-server".to_string(),
            args: vec!["--stdio".to_string(), "random arg".to_string()],
        };
        let server = "python";
        let rust_mapping = ("const".to_string(), "constant".to_string());
        let rust = lighter::LangName::from("rust");
        let mapping = HashMap::from([(rust.clone(), HashMap::from([rust_mapping.clone()]))]);
        let contents = format!(
            r#"
[servers]
{server} = "{} {} '{}'"

[captures.{rust}]
{} = "{}"
        "#,
            cmd.command, cmd.args[0], cmd.args[1], rust_mapping.0, rust_mapping.1
        );
        let file = temp_file("config.toml", &contents);
        let config = Config::from_file(&file.path()).unwrap();
        let parsed_command = config.commands.get(server).unwrap();

        assert_eq!(&cmd, parsed_command);
        assert_eq!(mapping, config.lang_mapping);
    }

    #[test]
    fn reject_empty_server_command() {
        let file = temp_file(
            ".toml",
            r#"
[servers]
rust = " "
"#,
        );

        let error = Config::from_file(&file.path()).err().unwrap();

        assert!(error.to_string().contains("Empty command string"));
    }
}
