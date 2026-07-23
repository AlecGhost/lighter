use std::cell::RefCell;

use clap::Parser;
use lighter::{config, daemon, theme};
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use thiserror::Error;

mod cli;

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
    #[error("Failed to resolve config path '{}'", .path.display())]
    ConfigPath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Config(#[from] config::Error),
    #[error(transparent)]
    Theme(#[from] theme::Error),
    #[error(transparent)]
    Daemon(#[from] daemon::Error),
    #[error(transparent)]
    Highlight(#[from] lighter::Error),
}

fn load_startup(options: &cli::StartupArgs) -> Result<(config::Config, arborium::theme::Theme)> {
    let config = config::Config::load(options.config.as_deref())?;
    let theme = theme::load(
        options.theme.as_deref(),
        options.custom_theme.as_deref(),
        config.theme.as_ref(),
    )?;
    Ok((config, theme))
}

fn highlight_once(options: &cli::Options, source: &str) -> Result<String> {
    let (config, theme) = load_startup(&options.startup)?;
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
            theme,
            lines: options.lines,
        },
        log,
    );
    highlighter
        .highlight(lighter::Input {
            source,
            path: options.file.as_deref(),
            project: options.project.as_deref(),
            lang: options.language.clone(),
        })
        .map_err(Error::from)
}

fn run_daemon(action: cli::DaemonAction) -> Result<()> {
    match action {
        cli::DaemonAction::Spawn { options } => {
            // load in order to fail fast
            let _startup = load_startup(&options)?;
            daemon::spawn(&cli::daemon_serve_arguments(&options)).map_err(Error::from)
        }
        cli::DaemonAction::Kill => daemon::kill().map_err(Error::from),
        cli::DaemonAction::Serve { options } => {
            let (config, theme) = load_startup(&options)?;
            let initial_options = daemon::Options {
                config,
                theme,
                format: options.format.unwrap_or_default(),
                log: options.log.unwrap_or_default(),
            };
            daemon::serve(initial_options).map_err(Error::from)
        }
    }
}

fn request_config_path(path: Option<&std::path::Path>) -> Result<Option<PathBuf>> {
    path.map(|path| {
        std::path::absolute(path).map_err(|source| Error::ConfigPath {
            path: path.to_owned(),
            source,
        })
    })
    .transpose()
}

fn run_once(options: cli::Options) -> Result<()> {
    let source = cli::read_input(options.file.as_deref())?;
    let output = match daemon::is_running() {
        true => {
            let request_theme = theme::Config::from_options(
                options.startup.theme.as_deref(),
                options.startup.custom_theme.as_deref(),
            )?;
            daemon::highlight(
                lighter::Input {
                    source: &source,
                    path: options.file.as_deref(),
                    project: options.project.as_deref(),
                    lang: options.language.clone(),
                },
                daemon::RequestOptions {
                    config: request_config_path(options.startup.config.as_deref())?,
                    output: options.startup.format,
                    theme: request_theme,
                    lsp: !options.no_lsp,
                    tree_sitter: !options.no_tree_sitter,
                    lines: options.lines,
                },
            )?
        }
        false => highlight_once(&options, &source)?,
    };
    print!("{output}");
    Ok(())
}

fn run() -> Result<()> {
    let cli = cli::Interface::parse();
    match cli.command {
        Some(cli::Command::Daemon { action }) => run_daemon(action),
        None => run_once(cli::Options::try_from(cli)?),
    }
}

fn write_error(mut output: impl io::Write, error: &Error) -> io::Result<()> {
    writeln!(output, "{error}")
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = write_error(io::stderr().lock(), &error);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    const STUB_FILE: &str = "stub.py";
    const CONFIG_FILE: &str = "lighter.toml";

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

    #[test]
    fn errors_are_rendered_as_text() {
        let error = Error::MissingLanguage;
        let mut output = Vec::new();

        write_error(&mut output, &error).unwrap();

        assert_eq!(String::from_utf8(output).unwrap(), format!("{error}\n"));
    }

    #[test]
    fn daemon_request_config_paths_are_absolute() {
        let path = request_config_path(Some(std::path::Path::new(CONFIG_FILE)))
            .unwrap()
            .unwrap();

        assert!(path.is_absolute());
        assert_eq!(path.file_name(), Some(OsStr::new(CONFIG_FILE)));
        assert_eq!(request_config_path(None).unwrap(), None);
    }
}
