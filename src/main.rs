use std::cell::RefCell;

use clap::Parser;
use lighter::daemon;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use thiserror::Error;

mod cli;
mod config;

type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
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

fn highlight_once(options: &cli::Options, source: &str) -> Result<String> {
    let config = config::Config::load(options.startup.config.as_deref())?.override_theme(
        options.startup.theme.as_deref(),
        options.startup.custom_theme.as_deref(),
    )?;
    let log = options.startup.log.unwrap_or_default();
    let registry = lighter::lsp::ServerRegistry::new(
        config.commands,
        config.general_mapping,
        config.lang_mapping,
        log,
    );
    let registry = Rc::new(RefCell::new(registry));
    let highlighter = lighter::Highlighter::with_options(
        registry,
        lighter::HighlightOptions {
            output: options.startup.format.unwrap_or_default(),
            lsp: !options.no_lsp,
            tree_sitter: !options.no_tree_sitter,
            theme: config.theme,
            lines: options.lines,
            project: options.project.clone(),
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

fn daemon_request(options: &cli::Options) -> daemon::RequestOptions {
    daemon::RequestOptions {
        project: options.project.clone(),
        lines: options.lines,
        no_tree_sitter: options.no_tree_sitter,
        no_lsp: options.no_lsp,
        format: options.startup.format,
    }
}

fn run_daemon(action: cli::DaemonAction) -> Result<()> {
    match action {
        cli::DaemonAction::Spawn { options } => {
            // load in order to fail fast
            let _config = config::Config::load(options.config.as_deref())?;
            daemon::spawn(&cli::daemon_serve_arguments(&options)).map_err(Error::from)
        }
        cli::DaemonAction::Kill => daemon::kill().map_err(Error::from),
        cli::DaemonAction::Serve { options } => {
            let config = config::Config::load(options.config.as_deref())?.override_theme(
                options.theme.as_deref(),
                options.custom_theme.as_deref(),
            )?;
            let initial_options = daemon::Options {
                commands: config.commands.clone(),
                general_mapping: config.general_mapping.clone(),
                lang_mapping: config.lang_mapping.clone(),
                theme: config.theme.clone(),
                format: options.format.unwrap_or_default(),
                log: options.log.unwrap_or_default(),
            };
            daemon::serve(initial_options).map_err(Error::from)
        }
    }
}

fn run_once(options: cli::Options) -> Result<()> {
    let source = cli::read_input(options.file.as_deref())?;
    let output = match daemon::is_running() {
        true => daemon::highlight(
            options.language.as_ref(),
            &source,
            &daemon_request(&options),
        )?,
        false => highlight_once(&options, &source)?,
    };
    print!("{output}");
    Ok(())
}

fn main() -> Result<()> {
    let cli = cli::Interface::parse();
    match cli.command {
        Some(cli::Command::Daemon { action }) => run_daemon(action),
        None => run_once(cli::Options::try_from(cli)?),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    const STUB_FILE: &str = "stub.py";

    fn parse_cli<T: AsRef<OsStr>>(args: &[T]) -> Result<cli::Interface> {
        cli::Interface::try_parse_from(
            std::iter::once(OsStr::new(cli::BIN_NAME)).chain(args.iter().map(AsRef::as_ref)),
        )
        .map_err(Error::from)
    }

    fn parse_options<T: AsRef<OsStr>>(args: &[T]) -> Result<cli::Options> {
        cli::Options::try_from(parse_cli(args)?)
    }

    #[test]
    fn one_time_highlighting_builds_a_local_highlighter() {
        let options = parse_options(&[
            STUB_FILE,
            cli::OptionName::NoLsp.flag().as_str(),
            cli::OptionName::NoTreeSitter.flag().as_str(),
            cli::OptionName::Lines.flag().as_str(),
            "1:1",
        ])
        .unwrap();

        let output = highlight_once(&options, "first\nsecond\n").unwrap();

        assert_eq!(output, "first");
    }
}
