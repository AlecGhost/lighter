use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

use lighter::logging;
use lighter::lsp;

use clap::Parser;
use thiserror::Error;

const BIN_NAME: &str = "lighter";
const STDIO_FRAME_TERMINATOR: u8 = b'\0';
const STDIO_LANGUAGE_SEPARATOR: char = '\n';
const STDIO_DIAGNOSTIC_PREFIX: &str = "lighter stdio:";

type Result<T> = std::result::Result<T, Error>;

#[derive(Parser, Debug)]
#[command(name = BIN_NAME, version, about)]
struct CliInterface {
    /// Source file to highlight (reads stdin when omitted).
    file: Option<PathBuf>,

    /// Run as a long-lived stdio server.
    /// Requests are `LANG\nSOURCE\0`; responses are `HIGHLIGHTED\0`. An empty
    /// request frame shuts down. Failed requests return an empty response and
    /// report the error on stderr. Cannot be combined with FILE or --lang.
    #[arg(long, conflicts_with_all = ["file", "lang"])]
    stdio: bool,

    /// Project directory exposed to the language server as its workspace.
    #[arg(short, long, value_name = "DIR")]
    project: Option<PathBuf>,

    /// Language name (e.g. "rust", "python").
    /// Inferred from file extension if a file path is provided.
    #[arg(short, long)]
    lang: Option<String>,

    /// Path to a TOML config file
    /// that configures the theme and maps language names to LSP server commands.
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

    /// Output highlighted ANSI, HTML, or LaTeX.
    #[arg(short, long, value_enum, default_value_t = lighter::Output::Ansi)]
    format: lighter::Output,

    /// Inclusive, one-based line range to output (start:end, :end, or start:).
    #[arg(long, value_name = "RANGE")]
    lines: Option<lighter::LineRange>,

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

#[derive(Error, Debug)]
enum Error {
    #[error(transparent)]
    Cli(#[from] clap::Error),
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
    #[error("Stdio server I/O failed")]
    StdioIo(#[source] io::Error),
    #[error(transparent)]
    Lsp(#[from] lsp::Error),
    #[error(transparent)]
    Highlight(#[from] lighter::Error),
}

/// Raw config maps directly to how the TOML is structured
#[derive(serde::Deserialize)]
struct RawConfig {
    /// Theme to use, either by built-in name or by path to a TOML theme file.
    #[serde(default)]
    theme: Option<ThemeConfig>,
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

#[derive(Debug, serde::Deserialize)]
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

/// A `RawConfig` parsed into runtime configuration.
#[derive(Debug)]
struct Config {
    commands: lsp::Commands,
    general_mapping: lsp::CaptureMapping,
    lang_mapping: lsp::LangCaptureMapping,
    theme: Option<ThemeConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            commands: lsp::default_commands(),
            general_mapping: lsp::CaptureMapping::new(),
            lang_mapping: lsp::LangCaptureMapping::new(),
            theme: None,
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
            theme: None,
        }
    }

    fn without_lsp(mut self) -> Self {
        self.commands.clear();
        self
    }

    /// Read a config from file, parses `RawConfig` and converts it into `Config`
    fn from_file(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|source| Error::ConfigRead {
            path: path.to_owned(),
            source,
        })?;
        let config: RawConfig = toml::from_str(&text).map_err(|source| Error::InvalidConfig {
            path: path.to_owned(),
            source,
        })?;

        let RawConfig {
            servers,
            captures,
            theme,
        } = config;

        // parse commands
        let config_commands = servers
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

                Ok((language, lsp::CommandEntry::new(program, args)))
            })
            .collect::<Result<Vec<_>>>()?;

        let mut commands = lsp::default_commands();
        commands.extend(config_commands);

        let mut general_mapping = HashMap::with_capacity(captures.len());
        let mut lang_mapping = HashMap::with_capacity(captures.len());
        for (key, val) in captures.into_iter() {
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
            theme: theme.map(|theme| theme.resolve_relative_to(path)),
        })
    }

    /// Load a config based on CLI arguments
    fn load(no_lsp: bool, config_path: Option<&Path>) -> Result<Config> {
        match (no_lsp, config_path) {
            (true, None) => Ok(Config::no_lsp()),
            (true, Some(path)) => Config::from_file(path).map(Config::without_lsp),
            (false, None) => Ok(Config::default()),
            (false, Some(path)) => Config::from_file(path),
        }
    }
}

#[derive(Debug)]
struct CliOptions {
    file: Option<PathBuf>,
    lang: Option<lighter::LangName>,
    stdio: bool,
    theme: arborium::theme::Theme,
    config: Config,
    project: Option<PathBuf>,
    log: logging::LogLevel,
    format: lighter::Output,
    lines: Option<lighter::LineRange>,
    no_tree_sitter: bool,
}

impl TryFrom<CliInterface> for CliOptions {
    type Error = Error;

    fn try_from(cli: CliInterface) -> Result<Self> {
        let lang = match cli.stdio {
            true => None,
            false => Some(CliOptions::resolve_language(
                cli.lang.as_deref(),
                cli.file.as_deref(),
            )?),
        };
        let config = Config::load(cli.no_lsp, cli.config.as_deref())?;
        let theme = CliOptions::load_theme(
            cli.theme.as_deref(),
            cli.custom_theme.as_deref(),
            config.theme.as_ref(),
        )?;
        let CliInterface {
            file,
            stdio,
            project,
            format,
            lines,
            no_tree_sitter,
            log,
            ..
        } = cli;
        Ok(Self {
            file,
            lang,
            stdio,
            theme,
            config,
            project,
            log,
            format,
            lines,
            no_tree_sitter,
        })
    }
}

impl CliOptions {
    /// Load theme based on CLI arguments
    fn load_theme(
        builtin_theme: Option<&str>,
        custom_path: Option<&Path>,
        config_theme: Option<&ThemeConfig>,
    ) -> Result<arborium::theme::Theme> {
        /// Load custom theme from file
        fn load_custom_theme(path: &Path) -> Result<arborium::theme::Theme> {
            let text = fs::read_to_string(path).map_err(|source| Error::ThemeRead {
                path: path.to_owned(),
                source,
            })?;
            arborium::theme::Theme::from_toml(&text).map_err(|source| Error::InvalidTheme {
                path: path.to_owned(),
                source,
            })
        }

        fn load_builtin_theme(name: &str) -> Result<arborium::theme::Theme> {
            arborium::theme::builtin::all()
                .into_iter()
                .find(|theme| theme.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| Error::UnknownBuiltinTheme(name.to_owned()))
        }

        match (builtin_theme, custom_path, config_theme) {
            (Some(name), None, _) => load_builtin_theme(name),
            (None, Some(path), _) => load_custom_theme(path),
            (None, None, Some(ThemeConfig::Builtin(name))) => load_builtin_theme(name),
            (None, None, Some(ThemeConfig::Custom { path })) => load_custom_theme(path),
            (None, None, None) => Ok(arborium_theme::builtin::catppuccin_mocha()),
            (Some(_), Some(_), _) => unreachable!("clap rejects conflicting theme options"),
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
                    .ok_or_else(|| Error::InvalidPath(file_name.to_owned()))?;
                arborium::detect_language(path)
                    .map(lighter::LangName::from)
                    .ok_or_else(|| Error::UnknownLanguage(file_name.to_owned()))
            }
            (None, None) => Err(Error::MissingLanguage),
        }
    }
}

/// Read input source code: from a file path or from stdin.
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
            let mut buf = String::new();
            stdin.read_to_string(&mut buf).map_err(Error::StdinRead)?;
            Ok(buf)
        }
    }
}

#[derive(Debug, Error)]
enum RequestFrameError {
    #[error("request frame is not valid UTF-8: {0}")]
    InvalidUtf8(#[from] std::str::Utf8Error),
    #[error("request frame is missing the language/source newline separator")]
    MissingLanguageSeparator,
    #[error("request frame has an empty language")]
    EmptyLanguage,
    #[error("request frame is missing its NUL terminator")]
    MissingTerminator,
}

fn parse_request_frame(frame: &[u8]) -> std::result::Result<(&str, &str), RequestFrameError> {
    let frame = std::str::from_utf8(frame)?;
    let (language, source) = frame
        .split_once(STDIO_LANGUAGE_SEPARATOR)
        .ok_or(RequestFrameError::MissingLanguageSeparator)?;

    match language.is_empty() {
        true => Err(RequestFrameError::EmptyLanguage),
        false => Ok((language, source)),
    }
}

fn write_stdio_response(mut output: impl Write, response: &str) -> io::Result<()> {
    output.write_all(response.as_bytes())?;
    output.write_all(&[STDIO_FRAME_TERMINATOR])?;
    output.flush()
}

fn report_stdio_error(
    mut diagnostics: impl Write,
    error: &impl std::fmt::Display,
) -> io::Result<()> {
    writeln!(diagnostics, "{STDIO_DIAGNOSTIC_PREFIX} {error}")
}

/// Serve NUL-delimited highlight requests until an empty frame or EOF.
///
/// A malformed request receives an empty response so clients retain one
/// response per non-shutdown frame. Its diagnostic is written separately.
fn serve_stdio<R, W, D, F, E>(
    mut input: R,
    mut output: W,
    mut diagnostics: D,
    mut highlight: F,
) -> io::Result<()>
where
    R: BufRead,
    W: Write,
    D: Write,
    F: FnMut(&str, &str) -> std::result::Result<String, E>,
    E: std::fmt::Display,
{
    let mut finished = false;
    let frames = std::iter::from_fn(|| match finished {
        true => None,
        false => {
            let mut frame = Vec::new();
            match input.read_until(STDIO_FRAME_TERMINATOR, &mut frame) {
                Ok(0) => {
                    finished = true;
                    None
                }
                Ok(_) => {
                    let terminated = frame.last() == Some(&STDIO_FRAME_TERMINATOR);
                    match terminated {
                        true => {
                            frame.pop();
                        }
                        false => finished = true,
                    }
                    Some(Ok((frame, terminated)))
                }
                Err(error) => {
                    finished = true;
                    Some(Err(error))
                }
            }
        }
    });

    frames
        .take_while(|frame| match frame {
            Ok((frame, terminated)) => !(frame.is_empty() && *terminated),
            Err(_) => true,
        })
        .try_for_each(|frame| {
            let (frame, terminated) = frame?;
            let response = match terminated {
                false => Err(RequestFrameError::MissingTerminator.to_string()),
                true => parse_request_frame(&frame)
                    .map_err(|error| error.to_string())
                    .and_then(|(language, source)| {
                        highlight(language, source).map_err(|error| error.to_string())
                    }),
            };

            match response {
                Ok(response) => write_stdio_response(&mut output, &response),
                Err(error) => {
                    report_stdio_error(&mut diagnostics, &error)?;
                    write_stdio_response(&mut output, "")
                }
            }
        })
}

fn main() -> Result<()> {
    let cli = CliInterface::parse();

    let options = CliOptions::try_from(cli)?;
    let registry = RefCell::new(lsp::ServerRegistry::new(
        options.config.commands,
        options.config.general_mapping,
        options.config.lang_mapping,
        options.project.as_deref(),
        options.log,
    )?);
    let highlighter = lighter::Highlighter::with_options(
        registry,
        lighter::HighlightOptions {
            output: options.format,
            tree_sitter: !options.no_tree_sitter,
            theme: options.theme,
            lines: options.lines,
        },
        options.log,
    );

    match options.stdio {
        true => serve_stdio(
            io::stdin().lock(),
            io::stdout().lock(),
            io::stderr().lock(),
            |language, source| {
                highlighter.highlight(lighter::Input {
                    source,
                    path: None,
                    lang: lighter::LangName::from(language),
                })
            },
        )
        .map_err(Error::StdioIo),
        false => {
            let source = read_input(options.file.as_deref())?;
            let input = lighter::Input {
                source: &source,
                path: options.file.as_deref(),
                lang: options
                    .lang
                    .expect("non-stdio mode always resolves a language"),
            };
            let output = highlighter.highlight(input)?;
            print!("{output}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck::{Arbitrary, Gen, TestResult};
    use std::assert_matches;

    const STUB_FILE: &str = "stub.py";
    const CONFIG_FILE_NAME: &str = "config.toml";
    const CUSTOM_THEME_NAME: &str = "Config custom theme";
    const UNKNOWN_THEME_NAME: &str = "unknown-theme";
    const CUSTOM_THEME_BODY: &str = r##"
variant = "light"

"keyword" = { fg = "accent" }

[palette]
accent = "#010203"
"##;

    enum CliArgs {
        Config,
        CustomTheme,
        Format,
        Lang,
        Lines,
        NoLsp,
        Project,
        Log,
        Stdio,
        Theme,
    }

    impl CliArgs {
        fn as_str(&self) -> &'static str {
            match self {
                CliArgs::Config => "--config",
                CliArgs::CustomTheme => "--custom-theme",
                CliArgs::Format => "--format",
                CliArgs::Lang => "--lang",
                CliArgs::Lines => "--lines",
                CliArgs::Log => "--log",
                CliArgs::NoLsp => "--no-lsp",
                CliArgs::Project => "--project",
                CliArgs::Stdio => "--stdio",
                CliArgs::Theme => "--theme",
            }
        }
    }

    trait TestValues: Clone + 'static {
        fn values() -> Vec<Self>;
    }

    #[derive(Clone, Debug)]
    struct TestValue<T>(T);

    impl<T> std::ops::Deref for TestValue<T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl<T: TestValues> Arbitrary for TestValue<T> {
        fn arbitrary(generator: &mut Gen) -> Self {
            let values = T::values();
            Self(generator.choose(&values).unwrap().clone())
        }
    }

    impl TestValues for arborium::theme::Theme {
        fn values() -> Vec<Self> {
            builtin_themes()
        }
    }

    impl TestValues for logging::LogLevel {
        fn values() -> Vec<Self> {
            <Self as clap::ValueEnum>::value_variants().to_vec()
        }
    }

    impl TestValues for lighter::Output {
        fn values() -> Vec<Self> {
            <Self as clap::ValueEnum>::value_variants().to_vec()
        }
    }

    type BuiltinTheme = TestValue<arborium::theme::Theme>;
    type ArbitraryLogLevel = TestValue<logging::LogLevel>;
    type ArbitraryOutput = TestValue<lighter::Output>;

    fn builtin_themes() -> Vec<arborium::theme::Theme> {
        arborium::theme::builtin::all()
    }

    fn is_builtin_theme(name: &str) -> bool {
        builtin_themes()
            .iter()
            .any(|theme| theme.name.eq_ignore_ascii_case(name))
    }

    fn temp_file(suffix: &str, source: &str) -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
        fs::write(file.path(), source).unwrap();
        file
    }

    fn missing_path(name: &str) -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        dir.path().join(name)
    }

    fn parse_options<T: AsRef<std::ffi::OsStr>>(args: &[T]) -> Result<CliOptions> {
        let cli = CliInterface::try_parse_from(
            std::iter::once(std::ffi::OsStr::new(BIN_NAME))
                .chain(args.iter().map(|arg| arg.as_ref())),
        )?;
        CliOptions::try_from(cli)
    }

    fn path_value(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    fn parse_cli_value(argument: CliArgs, value: &str) -> Result<CliOptions> {
        parse_options(&[STUB_FILE, argument.as_str(), value])
    }

    fn parse_builtin_theme(name: &str) -> Result<String> {
        parse_cli_value(CliArgs::Theme, name).map(|options| options.theme.name)
    }

    fn builtin_theme_config(name: &str) -> String {
        format!("theme = {name:?}\n")
    }

    fn custom_theme_config(path: &Path) -> String {
        format!("theme = {{ path = {:?} }}\n", path_value(path))
    }

    fn custom_theme_source() -> String {
        format!("name = {CUSTOM_THEME_NAME:?}\n{CUSTOM_THEME_BODY}")
    }

    quickcheck::quickcheck! {
        fn accept_builtin_theme(theme: BuiltinTheme) -> bool {
            parse_builtin_theme(&theme.name).is_ok_and(|name| theme.name == name)
        }

        fn accept_builtin_theme_case_insensitively(theme: BuiltinTheme) -> bool {
            parse_builtin_theme(&theme.name.to_lowercase())
                .is_ok_and(|name| theme.name == name)
        }

        fn reject_unknown_theme(name: String) -> TestResult {
            match name.starts_with('-') || is_builtin_theme(&name) {
                true => TestResult::discard(),
                false => TestResult::from_bool(matches!(
                    parse_builtin_theme(&name),
                    Err(Error::Cli(error))
                        if error.kind() == clap::error::ErrorKind::InvalidValue
                )),
            }
        }

        fn accept_log_level_case_insensitively(log_level: ArbitraryLogLevel) -> bool {
            parse_cli_value(CliArgs::Log, &log_level.as_str().to_lowercase())
                .is_ok_and(|options| options.log == *log_level)
        }

        fn accept_output_format(format: ArbitraryOutput) -> bool {
            let value = clap::ValueEnum::to_possible_value(&*format).unwrap();
            parse_cli_value(CliArgs::Format, value.get_name())
                .is_ok_and(|options| options.format == *format)
        }
    }

    #[test]
    fn accept_project_directory() {
        let dir = ".";
        let options = parse_options(&[STUB_FILE, CliArgs::Project.as_str(), dir]).unwrap();

        assert_eq!(options.project, Some(PathBuf::from(dir)));
    }

    #[test]
    fn accept_explicit_language() {
        let language = "rust";
        let options = parse_cli_value(CliArgs::Lang, language).unwrap();

        assert_eq!(options.lang.as_deref(), Some(language));
    }

    #[test]
    fn accept_stdio_without_a_language_and_reject_single_input_arguments() {
        let options = parse_options(&[CliArgs::Stdio.as_str()]).unwrap();

        assert!(options.stdio);
        assert!(options.lang.is_none());

        [
            vec![CliArgs::Stdio.as_str(), STUB_FILE],
            vec![CliArgs::Stdio.as_str(), CliArgs::Lang.as_str(), "rust"],
        ]
        .into_iter()
        .for_each(|args| {
            assert_matches!(
                parse_options(&args),
                Err(Error::Cli(error))
                    if error.kind() == clap::error::ErrorKind::ArgumentConflict
            );
        });
    }

    #[test]
    fn accept_line_range() {
        let range = "2:4";
        let options = parse_cli_value(CliArgs::Lines, range).unwrap();

        assert_eq!(options.lines, Some(range.parse().unwrap()));
    }

    #[test]
    fn reject_invalid_line_range() {
        let error = parse_cli_value(CliArgs::Lines, "4:2").unwrap_err();

        assert_matches!(
            error,
            Error::Cli(error) if error.kind() == clap::error::ErrorKind::ValueValidation
        );
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
        let options = parse_cli_value(CliArgs::Config, path_value(file.path())).unwrap();
        let parsed_command = options.config.commands.get(server).unwrap();

        assert_eq!(&cmd, parsed_command);
        assert_eq!(mapping, options.config.lang_mapping);
    }

    #[test]
    fn read_config_with_general_capture_mapping_from_disk() {
        let mapping = ("decorator".to_string(), "constant".to_string());
        let contents = format!(
            r#"
[captures]
{} = "{}"
"#,
            mapping.0, mapping.1
        );
        let file = temp_file("config.toml", &contents);

        let config = Config::from_file(file.path()).unwrap();

        assert_eq!(HashMap::from([mapping]), config.general_mapping);
        assert!(config.lang_mapping.is_empty());
    }

    #[test]
    fn accept_builtin_theme_from_config_case_insensitively() {
        let expected = builtin_themes().into_iter().next().unwrap();
        let file = temp_file(
            CONFIG_FILE_NAME,
            &builtin_theme_config(&expected.name.to_lowercase()),
        );

        let options = parse_cli_value(CliArgs::Config, path_value(file.path())).unwrap();

        assert_eq!(options.theme.name, expected.name);
    }

    #[test]
    fn accept_custom_theme_relative_to_config_with_lsp_disabled() {
        const THEME_FILE_NAME: &str = "theme.toml";
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join(THEME_FILE_NAME);
        fs::write(&theme_path, custom_theme_source()).unwrap();
        let config_path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(
            &config_path,
            custom_theme_config(Path::new(THEME_FILE_NAME)),
        )
        .unwrap();

        let options = parse_options(&[
            STUB_FILE,
            CliArgs::NoLsp.as_str(),
            CliArgs::Config.as_str(),
            path_value(&config_path),
        ])
        .unwrap();

        assert_eq!(options.theme.name, CUSTOM_THEME_NAME);
        assert!(options.config.commands.is_empty());
    }

    #[test]
    fn command_line_theme_overrides_config_theme() {
        let expected = builtin_themes().into_iter().next().unwrap();
        let config = temp_file(CONFIG_FILE_NAME, &builtin_theme_config(UNKNOWN_THEME_NAME));

        let options = parse_options(&[
            STUB_FILE,
            CliArgs::Config.as_str(),
            path_value(config.path()),
            CliArgs::Theme.as_str(),
            &expected.name,
        ])
        .unwrap();

        assert_eq!(options.theme.name, expected.name);
    }

    #[test]
    fn reject_unknown_builtin_theme_from_config() {
        let file = temp_file(CONFIG_FILE_NAME, &builtin_theme_config(UNKNOWN_THEME_NAME));

        let error = parse_cli_value(CliArgs::Config, path_value(file.path())).unwrap_err();

        assert_matches!(error, Error::UnknownBuiltinTheme(name) if name == UNKNOWN_THEME_NAME);
    }

    #[test]
    fn reject_invalid_theme_config_shape() {
        let file = temp_file(CONFIG_FILE_NAME, "theme = { name = \"invalid\" }");

        let error = parse_cli_value(CliArgs::Config, path_value(file.path())).unwrap_err();

        assert_matches!(error, Error::InvalidConfig { path, .. } if path == file.path());
    }

    #[test]
    fn accept_custom_theme_from_disk() {
        let expected = arborium_theme::builtin::catppuccin_mocha();
        let variant = match expected.is_dark {
            true => "dark",
            false => "light",
        };
        let source_url = expected.source_url.as_deref().unwrap();
        let background = expected.background.unwrap().to_hex();
        let foreground = expected.foreground.unwrap().to_hex();
        let keyword_index =
            arborium_theme::slot_to_highlight_index(arborium_theme::ThemeSlot::Keyword).unwrap();
        let keyword = expected.style(keyword_index).unwrap();
        let keyword_foreground = keyword.fg.unwrap().to_hex();

        let contents = format!(
            r##"
name = "{}"
variant = "{variant}"
source = "{source_url}"
background = "{background}"
foreground = "{foreground}"

"keyword" = {{ fg = "{keyword_foreground}" }}
"##,
            expected.name
        );
        let file = temp_file(".toml", &contents);

        let options = parse_cli_value(CliArgs::CustomTheme, path_value(file.path())).unwrap();
        let parsed_keyword = options.theme.style(keyword_index).unwrap();

        assert_eq!(expected.name, options.theme.name);
        assert_eq!(expected.is_dark, options.theme.is_dark);
        assert_eq!(expected.source_url, options.theme.source_url);
        assert_eq!(expected.background, options.theme.background);
        assert_eq!(expected.foreground, options.theme.foreground);
        assert_eq!(keyword.fg, parsed_keyword.fg);
        assert_eq!(keyword.bg, parsed_keyword.bg);
        assert_eq!(keyword.modifiers, parsed_keyword.modifiers);
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

        let error = parse_cli_value(CliArgs::Config, path_value(file.path())).unwrap_err();

        assert_matches!(error, Error::EmptyCommand(_));
    }

    #[test]
    fn reject_unreadable_config() {
        let path = missing_path("config.toml");

        let error = parse_cli_value(CliArgs::Config, path_value(&path)).unwrap_err();

        assert_matches!(error, Error::ConfigRead { path: error_path, .. } if error_path == path);
    }

    #[test]
    fn reject_invalid_config() {
        let file = temp_file(".toml", "=");

        let error = parse_cli_value(CliArgs::Config, path_value(file.path())).unwrap_err();

        assert_matches!(error, Error::InvalidConfig { path, .. } if path == file.path());
    }

    #[test]
    fn reject_invalid_server_command() {
        let file = temp_file(
            ".toml",
            r#"
[servers]
rust = "'"
"#,
        );

        let error = parse_cli_value(CliArgs::Config, path_value(file.path())).unwrap_err();

        assert_matches!(
            error,
            Error::InvalidCommand { language, command }
                if language.as_ref() == "rust" && command == "'"
        );
    }

    #[test]
    fn reject_unreadable_custom_theme() {
        let path = missing_path("theme.toml");

        let error = parse_cli_value(CliArgs::CustomTheme, path_value(&path)).unwrap_err();

        assert_matches!(error, Error::ThemeRead { path: error_path, .. } if error_path == path);
    }

    #[test]
    fn reject_invalid_custom_theme() {
        let file = temp_file(".toml", "=");

        let error = parse_cli_value(CliArgs::CustomTheme, path_value(file.path())).unwrap_err();

        assert_matches!(error, Error::InvalidTheme { path, .. } if path == file.path());
    }

    #[test]
    fn reject_unknown_builtin_theme() {
        let error = CliOptions::load_theme(Some(UNKNOWN_THEME_NAME), None, None).unwrap_err();

        assert_matches!(error, Error::UnknownBuiltinTheme(name) if name == UNKNOWN_THEME_NAME);
    }

    #[cfg(unix)]
    #[test]
    fn reject_non_utf8_file_path() {
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![0xff]));

        let error = parse_options(&[path.as_os_str()]).unwrap_err();

        assert_matches!(error, Error::InvalidPath(error_path) if error_path == path);
    }

    #[test]
    fn reject_file_with_unknown_language() {
        let path = "unknown-language";

        let error = parse_options(&[path]).unwrap_err();

        assert_matches!(error, Error::UnknownLanguage(error_path) if error_path == Path::new(path));
    }

    #[test]
    fn reject_missing_language() {
        let error = parse_options::<&str>(&[]).unwrap_err();

        assert_matches!(error, Error::MissingLanguage);
    }

    #[test]
    fn reject_unreadable_source_file() {
        let path = missing_path("source.rs");

        let error = read_input(Some(&path)).unwrap_err();

        assert_matches!(error, Error::SourceRead { path: error_path, .. } if error_path == path);
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::Other.into())
        }
    }

    #[test]
    fn reject_unreadable_stdin() {
        let error = read_input_from(None, FailingReader).unwrap_err();

        assert_matches!(error, Error::StdinRead(_));
    }

    struct ChunkedReader<R> {
        inner: R,
        chunk_size: usize,
    }

    impl<R: Read> Read for ChunkedReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let length = buf.len().min(self.chunk_size);
            self.inner.read(&mut buf[..length])
        }
    }

    fn request_frame(language: &str, source: &str) -> Vec<u8> {
        let mut frame = format!("{language}{STDIO_LANGUAGE_SEPARATOR}{source}").into_bytes();
        frame.push(STDIO_FRAME_TERMINATOR);
        frame
    }

    fn response_frame(response: &str) -> Vec<u8> {
        let mut frame = response.as_bytes().to_vec();
        frame.push(STDIO_FRAME_TERMINATOR);
        frame
    }

    #[test]
    fn stdio_serves_partial_multiple_utf8_frames_and_stops_on_an_empty_frame() {
        const FIRST_LANGUAGE: &str = "rust";
        const FIRST_SOURCE: &str = "let value = 1;";
        const SECOND_LANGUAGE: &str = "python";
        const SECOND_SOURCE: &str = "print('😀')";
        const IGNORED_SOURCE: &str = "ignored";
        let requests = [
            (FIRST_LANGUAGE, FIRST_SOURCE),
            (SECOND_LANGUAGE, SECOND_SOURCE),
        ];
        let mut input = requests
            .into_iter()
            .flat_map(|(language, source)| request_frame(language, source))
            .collect::<Vec<_>>();
        input.push(STDIO_FRAME_TERMINATOR);
        input.extend(request_frame(FIRST_LANGUAGE, IGNORED_SOURCE));
        let reader = io::BufReader::new(ChunkedReader {
            inner: io::Cursor::new(input),
            chunk_size: 1,
        });
        let mut output = Vec::new();
        let mut diagnostics = Vec::new();
        let mut received = Vec::new();

        serve_stdio(reader, &mut output, &mut diagnostics, |language, source| {
            received.push((language.to_owned(), source.to_owned()));
            Ok::<_, std::convert::Infallible>(format!("{language}:{source}"))
        })
        .unwrap();

        let expected = requests
            .into_iter()
            .flat_map(|(language, source)| response_frame(&format!("{language}:{source}")))
            .collect::<Vec<_>>();
        assert_eq!(output, expected);
        assert!(diagnostics.is_empty());
        assert_eq!(
            received,
            requests
                .into_iter()
                .map(|(language, source)| (language.to_owned(), source.to_owned()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn stdio_reports_malformed_frames_and_recovers_at_the_next_terminator() {
        const MISSING_SEPARATOR: &str = "missing-separator";
        const VALID_LANGUAGE: &str = "rust";
        const VALID_SOURCE: &str = "valid";
        const UNTERMINATED_SOURCE: &str = "unfinished";
        let mut input = response_frame(MISSING_SEPARATOR);
        input.extend(request_frame("", "empty language"));
        let invalid_utf8_body = format!("{VALID_LANGUAGE}{STDIO_LANGUAGE_SEPARATOR}")
            .bytes()
            .chain([0xff])
            .collect::<Vec<_>>();
        input.extend(
            invalid_utf8_body
                .iter()
                .copied()
                .chain([STDIO_FRAME_TERMINATOR]),
        );
        input.extend(request_frame(VALID_LANGUAGE, VALID_SOURCE));
        input.extend(
            format!("{VALID_LANGUAGE}{STDIO_LANGUAGE_SEPARATOR}{UNTERMINATED_SOURCE}").as_bytes(),
        );
        let mut output = Vec::new();
        let mut diagnostics = Vec::new();

        serve_stdio(
            io::Cursor::new(input),
            &mut output,
            &mut diagnostics,
            |_language, source| Ok::<_, std::convert::Infallible>(source.to_owned()),
        )
        .unwrap();

        let expected = ["", "", "", VALID_SOURCE, ""]
            .into_iter()
            .flat_map(response_frame)
            .collect::<Vec<_>>();
        assert_eq!(output, expected);
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(diagnostics.contains(&RequestFrameError::MissingLanguageSeparator.to_string()));
        assert!(diagnostics.contains(&RequestFrameError::EmptyLanguage.to_string()));
        let invalid_utf8 = parse_request_frame(&invalid_utf8_body)
            .unwrap_err()
            .to_string();
        assert!(diagnostics.contains(&invalid_utf8));
        assert!(diagnostics.contains(&RequestFrameError::MissingTerminator.to_string()));
    }

    #[test]
    fn stdio_reports_highlight_errors_and_continues_with_the_next_frame() {
        const LANGUAGE: &str = "rust";
        const FAILED_SOURCE: &str = "failed";
        const VALID_SOURCE: &str = "valid";
        const HIGHLIGHT_ERROR: &str = "highlight failed";
        let mut input = [FAILED_SOURCE, VALID_SOURCE]
            .into_iter()
            .flat_map(|source| request_frame(LANGUAGE, source))
            .collect::<Vec<_>>();
        input.push(STDIO_FRAME_TERMINATOR);
        let mut output = Vec::new();
        let mut diagnostics = Vec::new();

        serve_stdio(
            io::Cursor::new(input),
            &mut output,
            &mut diagnostics,
            |_language, source| match source {
                FAILED_SOURCE => Err(HIGHLIGHT_ERROR),
                source => Ok(source.to_owned()),
            },
        )
        .unwrap();

        let expected = ["", VALID_SOURCE]
            .into_iter()
            .flat_map(response_frame)
            .collect::<Vec<_>>();
        assert_eq!(output, expected);
        assert_eq!(
            String::from_utf8(diagnostics).unwrap(),
            format!("{STDIO_DIAGNOSTIC_PREFIX} {HIGHLIGHT_ERROR}\n")
        );
    }
}
