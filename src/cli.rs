use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{Error, Result};
use clap::ValueEnum;
use clap::{Args, Parser, Subcommand};
use lighter::logging;

pub const BIN_NAME: &str = "lighter";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandName {
    Daemon,
    Spawn,
    Kill,
    Serve,
}

impl CommandName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Spawn => "spawn",
            Self::Kill => "kill",
            Self::Serve => "serve",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptionName {
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
    pub const fn as_str(self) -> &'static str {
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

    pub const fn id(self) -> &'static str {
        match self {
            Self::CustomTheme => "custom_theme",
            Self::NoLsp => "no_lsp",
            Self::NoTreeSitter => "no_tree_sitter",
            option => option.as_str(),
        }
    }

    pub fn flag(self) -> String {
        format!("--{}", self.as_str())
    }
}

#[derive(Parser, Debug)]
#[command(name = BIN_NAME, version, about, args_conflicts_with_subcommands = true)]
pub struct Interface {
    #[command(subcommand)]
    pub command: Option<Command>,

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

    /// Disable LSP semantic highlighting.
    #[arg(long = OptionName::NoLsp.as_str())]
    no_lsp: bool,

    /// Disable tree-sitter syntax highlighting.
    #[arg(long = OptionName::NoTreeSitter.as_str())]
    no_tree_sitter: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage the background highlighting daemon.
    #[command(name = CommandName::Daemon.as_str())]
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
pub enum DaemonAction {
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

fn builtin_theme_parser() -> clap::builder::PossibleValuesParser {
    clap::builder::PossibleValuesParser::new(
        arborium::theme::builtin::all()
            .into_iter()
            .map(|theme| theme.name),
    )
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Args)]
pub struct StartupArgs {
    /// Path to a TOML config file.
    #[arg(short, long = OptionName::Config.as_str())]
    pub config: Option<PathBuf>,

    /// Name of a built-in Arborium theme.
    #[arg(
        long = OptionName::Theme.as_str(),
        conflicts_with = OptionName::CustomTheme.id(),
        ignore_case = true,
        value_parser = builtin_theme_parser()
    )]
    pub theme: Option<String>,

    /// Path to a custom TOML theme file.
    #[arg(
        long = OptionName::CustomTheme.as_str(),
        conflicts_with = OptionName::Theme.id()
    )]
    pub custom_theme: Option<PathBuf>,

    /// Output highlighted ANSI, HTML, LaTeX, or Typst.
    #[arg(short, long = OptionName::Format.as_str(), value_enum)]
    pub format: Option<lighter::Output>,

    /// Set stderr logging verbosity.
    #[arg(
        long = OptionName::Log.as_str(),
        value_enum,
        ignore_case = true
    )]
    pub log: Option<logging::LogLevel>,
}

#[derive(Debug)]
pub struct Options {
    pub file: Option<PathBuf>,
    pub language: lighter::LangName,
    pub project: Option<PathBuf>,
    pub lines: Option<lighter::LineRange>,
    pub no_lsp: bool,
    pub no_tree_sitter: bool,
    pub startup: StartupArgs,
}

impl TryFrom<Interface> for Options {
    type Error = Error;

    fn try_from(cli: Interface) -> Result<Self> {
        let language = resolve_language(cli.lang.as_deref(), cli.file.as_deref())?;
        Ok(Self {
            file: cli.file,
            language,
            project: cli.project,
            lines: cli.lines,
            no_lsp: cli.no_lsp,
            no_tree_sitter: cli.no_tree_sitter,
            startup: cli.startup,
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

pub fn daemon_serve_arguments(options: &StartupArgs) -> Vec<OsString> {
    fn push_option(arguments: &mut Vec<OsString>, option: OptionName, value: &OsStr) {
        arguments.extend([OsString::from(option.flag()), value.to_owned()]);
    }

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

pub fn read_input(path: Option<&Path>) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    const STUB_FILE: &str = "stub.py";

    fn parse_cli<T: AsRef<OsStr>>(args: &[T]) -> Result<Interface> {
        Interface::try_parse_from(
            std::iter::once(OsStr::new(BIN_NAME)).chain(args.iter().map(AsRef::as_ref)),
        )
        .map_err(Error::from)
    }

    fn parse_options<T: AsRef<OsStr>>(args: &[T]) -> Result<Options> {
        Options::try_from(parse_cli(args)?)
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
        assert!(options.no_lsp);
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
            Some(Command::Daemon {
                action: DaemonAction::Spawn { options }
            }) if options == StartupArgs::default()
        );
        assert_matches!(
            parse_cli(&[CommandName::Daemon.as_str(), CommandName::Kill.as_str(),])
                .unwrap()
                .command,
            Some(Command::Daemon {
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
            log: Some(logging::LogLevel::Debug),
            ..Default::default()
        };

        let arguments = daemon_serve_arguments(&options);
        let cli =
            Interface::try_parse_from(std::iter::once(OsString::from(BIN_NAME)).chain(arguments))
                .unwrap();

        assert_matches!(
            cli.command,
            Some(Command::Daemon {
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
}
