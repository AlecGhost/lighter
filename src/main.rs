use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use lighter::{daemon, logging};
use serde::Deserialize;
use thiserror::Error;

const BIN_NAME: &str = "lighter";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandName {
    Daemon,
    Spawn,
    Kill,
    Serve,
}

impl CommandName {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Spawn => "spawn",
            Self::Kill => "kill",
            Self::Serve => "serve",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OptionName {
    Config,
    Theme,
    CustomTheme,
    Format,
    Project,
    Lang,
    Lines,
    NoLsp,
    NoTreeSitter,
    Log,
}

impl OptionName {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Theme => "theme",
            Self::CustomTheme => "custom-theme",
            Self::Format => "format",
            Self::Project => "project",
            Self::Lang => "lang",
            Self::Lines => "lines",
            Self::NoLsp => "no-lsp",
            Self::NoTreeSitter => "no-tree-sitter",
            Self::Log => "log",
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::CustomTheme => "custom_theme",
            Self::NoLsp => "no_lsp",
            Self::NoTreeSitter => "no_tree_sitter",
            option => option.as_str(),
        }
    }

    fn flag(self) -> String {
        format!("--{}", self.as_str())
    }
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Parser, Debug)]
#[command(name = BIN_NAME, version, about, args_conflicts_with_subcommands = true)]
struct CliInterface {
    #[command(subcommand)]
    command: Option<CliCommand>,

    /// Source file to highlight (reads stdin when omitted).
    file: Option<PathBuf>,

    /// Project directory exposed to the language server as its workspace.
    #[arg(short, long = OptionName::Project.as_str(), value_name = "DIR")]
    project: Option<PathBuf>,

    /// Language name (e.g. "rust", "python").
    /// Inferred from file extension if a file path is provided.
    #[arg(short, long = OptionName::Lang.as_str())]
    lang: Option<String>,

    #[command(flatten)]
    startup: StartupArgs,

    /// Inclusive, one-based line range to output (start:end, :end, or start:).
    #[arg(long = OptionName::Lines.as_str(), value_name = "RANGE")]
    lines: Option<lighter::LineRange>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Manage the background highlighting daemon.
    #[command(name = CommandName::Daemon.as_str())]
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
enum DaemonAction {
    /// Spawn the background daemon.
    #[command(name = CommandName::Spawn.as_str())]
    Spawn {
        #[command(flatten)]
        options: StartupArgs,
    },
    /// Kill the background daemon.
    #[command(name = CommandName::Kill.as_str())]
    Kill,
    #[command(name = CommandName::Serve.as_str(), hide = true)]
    Serve {
        #[command(flatten)]
        options: StartupArgs,
    },
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Args)]
struct StartupArgs {
    /// Path to a TOML config file.
    #[arg(short, long = OptionName::Config.as_str())]
    config: Option<PathBuf>,

    /// Name of a built-in Arborium theme.
    #[arg(
        long = OptionName::Theme.as_str(),
        conflicts_with = OptionName::CustomTheme.id(),
        ignore_case = true,
        value_parser = lighter::builtin_theme_parser()
    )]
    theme: Option<String>,

    /// Path to a custom TOML theme file.
    #[arg(
        long = OptionName::CustomTheme.as_str(),
        conflicts_with = OptionName::Theme.id()
    )]
    custom_theme: Option<PathBuf>,

    /// Output highlighted ANSI, HTML, or LaTeX.
    #[arg(short, long = OptionName::Format.as_str(), value_enum)]
    format: Option<lighter::Output>,

    /// Disable LSP semantic highlighting.
    #[arg(long = OptionName::NoLsp.as_str())]
    no_lsp: bool,

    /// Disable tree-sitter syntax highlighting.
    #[arg(long = OptionName::NoTreeSitter.as_str())]
    no_tree_sitter: bool,

    /// Set stderr logging verbosity.
    #[arg(
        long = OptionName::Log.as_str(),
        value_enum,
        ignore_case = true
    )]
    log: Option<logging::LogLevel>,
}

impl StartupArgs {
    fn resolve_paths(mut self) -> Result<Self> {
        self.config = canonicalize_path(self.config, |path, source| Error::ConfigRead {
            path,
            source,
        })?;
        self.custom_theme = canonicalize_path(self.custom_theme, |path, source| {
            Error::ThemeRead { path, source }
        })?;
        Ok(self)
    }
}

fn canonicalize_path(
    path: Option<PathBuf>,
    error: impl FnOnce(PathBuf, io::Error) -> Error,
) -> Result<Option<PathBuf>> {
    path.map(|path| fs::canonicalize(&path).map_err(|source| error(path, source)))
        .transpose()
}

#[derive(Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    theme: Option<ThemeConfig>,
    #[serde(default)]
    servers: HashMap<String, String>,
    #[serde(default)]
    captures: HashMap<String, CaptureMapping>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CaptureMapping {
    Capture(String),
    Language(HashMap<String, String>),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum ThemeConfig {
    Builtin(String),
    Custom { path: PathBuf },
}

impl ThemeConfig {
    fn resolve_relative_to(self, config_path: &Path) -> Self {
        match self {
            Self::Custom { path } if path.is_relative() => Self::Custom {
                path: config_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(path),
            },
            theme => theme,
        }
    }
}

#[derive(Clone, Debug)]
struct Config {
    path: Option<PathBuf>,
    commands: lighter::lsp::Commands,
    general_mapping: lighter::lsp::CaptureMapping,
    lang_mapping: lighter::lsp::LangCaptureMapping,
    theme: arborium::theme::Theme,
}

impl Config {
    fn load(options: &StartupArgs) -> Result<Self> {
        let raw = options
            .config
            .as_deref()
            .map(Self::read)
            .transpose()?
            .unwrap_or_default();
        let RawConfig {
            servers,
            captures,
            theme,
        } = raw;
        let configured_commands = servers
            .into_iter()
            .map(|(language, command)| {
                let language = lighter::LangName::from(language);
                let parts = shlex::split(&command).ok_or_else(|| Error::InvalidCommand {
                    language: language.clone(),
                    command,
                })?;
                let Some((program, args)) = parts.split_first() else {
                    return Err(Error::EmptyCommand(language));
                };
                Ok((language, lighter::lsp::CommandEntry::new(program, args)))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut commands = lighter::lsp::default_commands();
        commands.extend(configured_commands);
        let mut general_mapping = lighter::lsp::CaptureMapping::with_capacity(captures.len());
        let mut lang_mapping = lighter::lsp::LangCaptureMapping::with_capacity(captures.len());
        captures
            .into_iter()
            .for_each(|(capture, mapping)| match mapping {
                CaptureMapping::Capture(mapping) => {
                    general_mapping.insert(capture, mapping);
                }
                CaptureMapping::Language(mapping) => {
                    lang_mapping.insert(lighter::LangName::from(capture), mapping);
                }
            });
        let theme = load_theme(
            options.theme.as_deref(),
            options.custom_theme.as_deref(),
            theme.as_ref(),
        )?;
        Ok(Self {
            path: options.config.clone(),
            commands,
            general_mapping,
            lang_mapping,
            theme,
        })
    }

    fn read(path: &Path) -> Result<RawConfig> {
        let text = fs::read_to_string(path).map_err(|source| Error::ConfigRead {
            path: path.to_owned(),
            source,
        })?;
        let mut raw =
            toml::from_str::<RawConfig>(&text).map_err(|source| Error::InvalidConfig {
                path: path.to_owned(),
                source,
            })?;
        raw.theme = raw.theme.map(|theme| theme.resolve_relative_to(path));
        Ok(raw)
    }

    fn daemon_options(&self, defaults: &StartupArgs) -> daemon::Options {
        daemon::Options {
            config: self.path.clone(),
            commands: self.commands.clone(),
            general_mapping: self.general_mapping.clone(),
            lang_mapping: self.lang_mapping.clone(),
            theme: self.theme.clone(),
            format: defaults.format,
            no_lsp: defaults.no_lsp,
            no_tree_sitter: defaults.no_tree_sitter,
            log: defaults.log.unwrap_or_default(),
        }
    }
}

fn load_theme(
    builtin_theme: Option<&str>,
    custom_path: Option<&Path>,
    config_theme: Option<&ThemeConfig>,
) -> Result<arborium::theme::Theme> {
    fn custom(path: &Path) -> Result<arborium::theme::Theme> {
        let text = fs::read_to_string(path).map_err(|source| Error::ThemeRead {
            path: path.to_owned(),
            source,
        })?;
        arborium::theme::Theme::from_toml(&text).map_err(|source| Error::InvalidTheme {
            path: path.to_owned(),
            source,
        })
    }

    fn builtin(name: &str) -> Result<arborium::theme::Theme> {
        arborium::theme::builtin::all()
            .into_iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::UnknownBuiltinTheme(name.to_owned()))
    }

    match (builtin_theme, custom_path, config_theme) {
        (Some(name), None, _) => builtin(name),
        (None, Some(path), _) => custom(path),
        (None, None, Some(ThemeConfig::Builtin(name))) => builtin(name),
        (None, None, Some(ThemeConfig::Custom { path })) => custom(path),
        (None, None, None) => Ok(arborium_theme::builtin::catppuccin_mocha()),
        (Some(_), Some(_), _) => unreachable!("CLI rejects conflicting theme options"),
    }
}

#[derive(Error, Debug)]
enum Error {
    #[error(transparent)]
    Cli(#[from] clap::Error),
    #[error("Invalid path {}", .0.display())]
    InvalidPath(PathBuf),
    #[error(
        "Could not detect language from file name '{}'. Specify it with --lang flag.",
        .0.display()
    )]
    UnknownLanguage(PathBuf),
    #[error("Could not detect language. Specify it with --lang flag.")]
    MissingLanguage,
    #[error("Failed to read source file '{}'", .path.display())]
    SourceRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Failed to read stdin")]
    StdinRead(#[source] io::Error),
    #[error("Failed to read config file '{}'", .path.display())]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Invalid toml in config file '{}'", .path.display())]
    InvalidConfig {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("Invalid command string for language '{language}': {command}")]
    InvalidCommand {
        language: lighter::LangName,
        command: String,
    },
    #[error("Empty command string for language '{0}'")]
    EmptyCommand(lighter::LangName),
    #[error("Failed to read theme file '{}'", .path.display())]
    ThemeRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Invalid theme file '{}'", .path.display())]
    InvalidTheme {
        path: PathBuf,
        #[source]
        source: arborium_theme::ThemeError,
    },
    #[error("Unknown built-in theme '{0}'")]
    UnknownBuiltinTheme(String),
    #[error(transparent)]
    Daemon(#[from] daemon::Error),
    #[error(transparent)]
    Highlight(#[from] lighter::Error),
}

#[derive(Debug)]
struct CliOptions {
    file: Option<PathBuf>,
    language: lighter::LangName,
    project: Option<PathBuf>,
    lines: Option<lighter::LineRange>,
    startup: StartupArgs,
}

impl CliOptions {
    fn daemon_request(&self) -> daemon::RequestOptions {
        daemon::RequestOptions {
            project: self.project.clone(),
            lines: self.lines,
            no_tree_sitter: self.startup.no_tree_sitter,
            no_lsp: self.startup.no_lsp,
            format: self.startup.format,
            config: self.startup.config.clone(),
        }
    }
}

impl TryFrom<CliInterface> for CliOptions {
    type Error = Error;

    fn try_from(cli: CliInterface) -> Result<Self> {
        let language = resolve_language(cli.lang.as_deref(), cli.file.as_deref())?;
        let startup = cli.startup.resolve_paths()?;
        Ok(Self {
            file: cli.file,
            language,
            project: cli.project,
            lines: cli.lines,
            startup,
        })
    }
}

fn resolve_language(lang: Option<&str>, file: Option<&Path>) -> Result<lighter::LangName> {
    match (lang, file) {
        (Some(lang), _) => Ok(lighter::LangName::from(lang)),
        (None, Some(file)) => {
            let path = file
                .to_str()
                .ok_or_else(|| Error::InvalidPath(file.to_owned()))?;
            arborium::detect_language(path)
                .map(lighter::LangName::from)
                .ok_or_else(|| Error::UnknownLanguage(file.to_owned()))
        }
        (None, None) => Err(Error::MissingLanguage),
    }
}

fn read_input(path: Option<&Path>) -> Result<String> {
    read_input_from(path, io::stdin())
}

fn read_input_from(path: Option<&Path>, mut stdin: impl Read) -> Result<String> {
    match path {
        Some(path) => fs::read_to_string(path).map_err(|source| Error::SourceRead {
            path: path.to_owned(),
            source,
        }),
        None => {
            let mut source = String::new();
            stdin
                .read_to_string(&mut source)
                .map_err(Error::StdinRead)?;
            Ok(source)
        }
    }
}

fn push_option(arguments: &mut Vec<OsString>, option: OptionName, value: &OsStr) {
    arguments.extend([OsString::from(option.flag()), value.to_owned()]);
}

fn daemon_serve_arguments(options: &StartupArgs) -> Vec<OsString> {
    let mut arguments = vec![
        CommandName::Daemon.as_str().into(),
        CommandName::Serve.as_str().into(),
    ];
    if let Some(path) = &options.config {
        push_option(&mut arguments, OptionName::Config, path.as_os_str());
    }
    if let Some(theme) = &options.theme {
        push_option(&mut arguments, OptionName::Theme, OsStr::new(theme));
    }
    if let Some(path) = &options.custom_theme {
        push_option(&mut arguments, OptionName::CustomTheme, path.as_os_str());
    }
    if let Some(format) = options.format {
        let value = format.to_possible_value().expect("output has a value");
        push_option(
            &mut arguments,
            OptionName::Format,
            OsStr::new(value.get_name()),
        );
    }
    if options.no_lsp {
        arguments.push(OptionName::NoLsp.flag().into());
    }
    if options.no_tree_sitter {
        arguments.push(OptionName::NoTreeSitter.flag().into());
    }
    if let Some(log) = options.log {
        let value = log.to_possible_value().expect("log level has a value");
        push_option(
            &mut arguments,
            OptionName::Log,
            OsStr::new(value.get_name()),
        );
    }
    arguments
}

fn run_once(options: CliOptions) -> Result<()> {
    let source = read_input(options.file.as_deref())?;
    let output = match daemon::is_running() {
        true => daemon::highlight(
            options.language.as_ref(),
            &source,
            &options.daemon_request(),
        )?,
        false => highlight_once(&options, &source)?,
    };
    print!("{output}");
    Ok(())
}

fn highlight_once(options: &CliOptions, source: &str) -> Result<String> {
    let config = Config::load(&options.startup)?;
    let log = options.startup.log.unwrap_or_default();
    let registry = lighter::lsp::ServerRegistry::new(
        config.commands,
        config.general_mapping,
        config.lang_mapping,
        options.project.as_deref(),
        log,
    )
    .map_err(lighter::Error::from)?;
    let highlighter = lighter::Highlighter::with_options(
        RefCell::new(registry),
        lighter::HighlightOptions {
            output: options.startup.format.unwrap_or_default(),
            lsp: !options.startup.no_lsp,
            tree_sitter: !options.startup.no_tree_sitter,
            theme: config.theme,
            lines: options.lines,
        },
        log,
    );
    highlighter
        .highlight(lighter::Input {
            source,
            path: options.file.as_deref(),
            lang: options.language.clone(),
        })
        .map_err(Error::from)
}

fn run_daemon(action: DaemonAction) -> Result<()> {
    match action {
        DaemonAction::Spawn { options } => {
            let options = options.resolve_paths()?;
            Config::load(&options)?;
            daemon::spawn(&daemon_serve_arguments(&options)).map_err(Error::from)
        }
        DaemonAction::Kill => daemon::kill().map_err(Error::from),
        DaemonAction::Serve { options } => {
            let options = options.resolve_paths()?;
            let config = Config::load(&options)?;
            let daemon_options = config.daemon_options(&options);
            let defaults = options.clone();
            daemon::serve(daemon_options, move |path| {
                let mut options = defaults.clone();
                options.config = Some(path.to_owned());
                Config::load(&options).map(|config| config.daemon_options(&options))
            })
            .map_err(Error::from)
        }
    }
}

fn main() -> Result<()> {
    let cli = CliInterface::parse();
    match cli.command {
        Some(CliCommand::Daemon { action }) => run_daemon(action),
        None => run_once(CliOptions::try_from(cli)?),
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    const STUB_FILE: &str = "stub.py";
    const CONFIG_FILE: &str = "config.toml";

    fn config_file(source: &str) -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new()
            .suffix(CONFIG_FILE)
            .tempfile()
            .unwrap();
        fs::write(file.path(), source).unwrap();
        file
    }

    fn config_from_source(source: &str) -> Result<Config> {
        let file = config_file(source);
        Config::load(&StartupArgs {
            config: Some(file.path().to_owned()),
            ..Default::default()
        })
    }

    fn parse_cli<T: AsRef<OsStr>>(args: &[T]) -> Result<CliInterface> {
        CliInterface::try_parse_from(
            std::iter::once(OsStr::new(BIN_NAME)).chain(args.iter().map(AsRef::as_ref)),
        )
        .map_err(Error::from)
    }

    fn parse_options<T: AsRef<OsStr>>(args: &[T]) -> Result<CliOptions> {
        CliOptions::try_from(parse_cli(args)?)
    }

    #[test]
    fn parses_one_time_options() {
        let args = [
            OsString::from(STUB_FILE),
            OptionName::Project.flag().into(),
            ".".into(),
            OptionName::Lines.flag().into(),
            "2:4".into(),
            OptionName::Format.flag().into(),
            "html".into(),
            OptionName::NoLsp.flag().into(),
            OptionName::Lang.flag().into(),
            "python".into(),
        ];
        let options = parse_options(&args).unwrap();

        assert_eq!(options.language.as_ref(), "python");
        assert_eq!(options.project, Some(PathBuf::from(".")));
        assert_eq!(options.lines, Some("2:4".parse().unwrap()));
        assert_eq!(options.startup.format, Some(lighter::Output::Html));
        assert!(options.startup.no_lsp);
    }

    #[test]
    fn parses_only_dedicated_daemon_subcommands() {
        assert_matches!(
            parse_cli(&[
                CommandName::Daemon.as_str(),
                CommandName::Spawn.as_str(),
            ])
            .unwrap()
            .command,
            Some(CliCommand::Daemon {
                action: DaemonAction::Spawn { options }
            }) if options == StartupArgs::default()
        );
        assert_matches!(
            parse_cli(&[CommandName::Daemon.as_str(), CommandName::Kill.as_str(),])
                .unwrap()
                .command,
            Some(CliCommand::Daemon {
                action: DaemonAction::Kill
            })
        );
        assert!(
            parse_cli(&[
                CommandName::Daemon.as_str(),
                CommandName::Spawn.as_str(),
                STUB_FILE,
            ])
            .is_err()
        );
        let invalid_daemon_option = format!("--{}", CommandName::Daemon.as_str());
        assert!(
            parse_cli(&[invalid_daemon_option.as_str(), CommandName::Spawn.as_str(),]).is_err()
        );
    }

    #[test]
    fn parses_builtin_themes_case_insensitively() {
        arborium::theme::builtin::all()
            .into_iter()
            .for_each(|theme| {
                let name = theme.name.to_lowercase();
                let args = [
                    OsString::from(STUB_FILE),
                    OptionName::Theme.flag().into(),
                    name.clone().into(),
                ];

                let options = parse_options(&args).unwrap();

                assert_eq!(options.startup.theme.as_deref(), Some(name.as_str()));
            });
    }

    #[test]
    fn rejects_unknown_theme_and_invalid_line_range() {
        const UNKNOWN_THEME: &str = "unknown-theme";
        let theme_args = [
            OsString::from(STUB_FILE),
            OptionName::Theme.flag().into(),
            UNKNOWN_THEME.into(),
        ];
        let line_args = [
            OsString::from(STUB_FILE),
            OptionName::Lines.flag().into(),
            "4:2".into(),
        ];

        [theme_args, line_args]
            .into_iter()
            .for_each(|args| assert_matches!(parse_options(&args), Err(Error::Cli(_))));
    }

    #[test]
    fn serializes_daemon_startup_options() {
        let theme = arborium::theme::builtin::all()
            .into_iter()
            .next()
            .expect("at least one built-in theme");
        let options = StartupArgs {
            theme: Some(theme.name),
            format: Some(lighter::Output::Html),
            no_lsp: true,
            no_tree_sitter: true,
            log: Some(logging::LogLevel::Debug),
            ..Default::default()
        };

        let arguments = daemon_serve_arguments(&options);
        let cli = CliInterface::try_parse_from(
            std::iter::once(OsString::from(BIN_NAME)).chain(arguments),
        )
        .unwrap();

        assert_matches!(
            cli.command,
            Some(CliCommand::Daemon {
                action: DaemonAction::Serve { options: parsed }
            }) if parsed == options
        );
    }

    #[test]
    fn rejects_missing_or_unknown_language() {
        assert_matches!(parse_options::<&str>(&[]), Err(Error::MissingLanguage));
        assert_matches!(
            parse_options(&["unknown-language"]),
            Err(Error::UnknownLanguage(path)) if path == Path::new("unknown-language")
        );
    }

    #[test]
    fn rejects_unreadable_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing.rs");

        let error = read_input(Some(&path)).unwrap_err();

        assert_matches!(error, Error::SourceRead { path: error_path, .. } if error_path == path);
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::Other.into())
        }
    }

    #[test]
    fn rejects_unreadable_stdin() {
        assert_matches!(
            read_input_from(None, FailingReader),
            Err(Error::StdinRead(_))
        );
    }

    #[test]
    fn loads_commands_and_capture_mappings_from_config() {
        const SERVER: &str = "custom-server";
        const CAPTURE: &str = "decorator";
        const MAPPING: &str = "constant";
        let config = config_from_source(&format!(
            r#"
[servers]
python = "{SERVER} --stdio 'multi word'"

[captures]
{CAPTURE} = "{MAPPING}"
"#,
        ))
        .unwrap();
        let command = config.commands.get("python").unwrap();

        assert_eq!(command.command, SERVER);
        assert_eq!(command.args, ["--stdio", "multi word"]);
        assert_eq!(config.general_mapping.get(CAPTURE).unwrap(), MAPPING);
    }

    #[test]
    fn resolves_custom_theme_relative_to_config() {
        const THEME_FILE: &str = "theme.toml";
        const THEME_NAME: &str = "Relative theme";
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(THEME_FILE),
            format!(
                r##"
name = "{THEME_NAME}"
variant = "dark"
"keyword" = {{ fg = "accent" }}

[palette]
accent = "#010203"
"##,
            ),
        )
        .unwrap();
        let config_path = directory.path().join(CONFIG_FILE);
        fs::write(
            &config_path,
            format!("theme = {{ path = {THEME_FILE:?} }}\n"),
        )
        .unwrap();

        let config = Config::load(&StartupArgs {
            config: Some(config_path),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(config.theme.name, THEME_NAME);
    }

    #[test]
    fn command_line_theme_overrides_config_theme() {
        const UNKNOWN_THEME: &str = "unknown-theme";
        let expected = arborium::theme::builtin::all()
            .into_iter()
            .next()
            .expect("at least one built-in theme");
        let file = config_file(&format!("theme = {UNKNOWN_THEME:?}\n"));

        let config = Config::load(&StartupArgs {
            config: Some(file.path().to_owned()),
            theme: Some(expected.name.clone()),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(config.theme.name, expected.name);
    }

    #[test]
    fn rejects_invalid_config_and_server_commands() {
        let invalid_toml = config_from_source("=").unwrap_err();
        let empty_command = config_from_source("[servers]\nrust = ' '").unwrap_err();
        let invalid_command = config_from_source("[servers]\nrust = \"'\"").unwrap_err();

        assert!(matches!(invalid_toml, Error::InvalidConfig { .. }));
        assert!(matches!(empty_command, Error::EmptyCommand(_)));
        assert!(matches!(invalid_command, Error::InvalidCommand { .. }));
    }

    #[test]
    fn one_time_highlighting_builds_a_local_highlighter() {
        let options = parse_options(&[
            STUB_FILE,
            OptionName::NoLsp.flag().as_str(),
            OptionName::NoTreeSitter.flag().as_str(),
            OptionName::Lines.flag().as_str(),
            "1:1",
        ])
        .unwrap();

        let output = highlight_once(&options, "first\nsecond\n").unwrap();

        assert_eq!(output, "first");
    }
}
