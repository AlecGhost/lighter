use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::{LangName, lsp, theme};

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to read config file '{}'", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Invalid toml in config file '{}'", .path.display())]
    Invalid {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("Invalid command string for language '{language}': {command}")]
    InvalidCommand { language: LangName, command: String },
    #[error("Empty command string for language '{0}'")]
    EmptyCommand(LangName),
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    theme: Option<theme::Config>,
    #[serde(default)]
    servers: HashMap<String, ServerEntry>,
    #[serde(default)]
    captures: HashMap<String, CaptureMapping>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ServerEntry {
    Command(String),
    Detailed {
        command: String,
        #[serde(default = "empty_server_configuration")]
        config: lsp::ServerConfiguration,
    },
}

impl ServerEntry {
    fn into_parts(self) -> (String, lsp::ServerConfiguration) {
        match self {
            Self::Command(command) => (command, empty_server_configuration()),
            Self::Detailed { command, config } => (command, config),
        }
    }
}

fn empty_server_configuration() -> lsp::ServerConfiguration {
    serde_json::json!({})
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CaptureMapping {
    Capture(String),
    Language(HashMap<String, String>),
}

#[derive(Clone, Debug)]
pub struct Config {
    pub commands: lsp::Commands,
    pub general_mapping: lsp::CaptureMapping,
    pub lang_mapping: lsp::LangCaptureMapping,
    pub theme: Option<theme::Config>,
}

impl Config {
    pub fn load(config_path: Option<&Path>) -> Result<Self> {
        let raw = config_path.map(Self::read).transpose()?.unwrap_or_default();
        let RawConfig {
            servers,
            captures,
            theme: config_theme,
        } = raw;
        let configured_commands = servers
            .into_iter()
            .map(|(language, server)| {
                let language = LangName::from(language);
                let (command, configuration) = server.into_parts();
                let parts = shlex::split(&command).ok_or_else(|| Error::InvalidCommand {
                    language: language.clone(),
                    command,
                })?;
                let Some((program, args)) = parts.split_first() else {
                    return Err(Error::EmptyCommand(language));
                };
                Ok((
                    language,
                    lsp::CommandEntry::new(program, args, configuration),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut commands = lsp::default_commands();
        commands.extend(configured_commands);
        let mut general_mapping = lsp::CaptureMapping::with_capacity(captures.len());
        let mut lang_mapping = lsp::LangCaptureMapping::with_capacity(captures.len());
        captures
            .into_iter()
            .for_each(|(capture, mapping)| match mapping {
                CaptureMapping::Capture(mapping) => {
                    general_mapping.insert(capture, mapping);
                }
                CaptureMapping::Language(mapping) => {
                    lang_mapping.insert(LangName::from(capture), mapping);
                }
            });
        Ok(Self {
            commands,
            general_mapping,
            lang_mapping,
            theme: config_theme,
        })
    }

    fn read(path: &Path) -> Result<RawConfig> {
        let text = fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.to_owned(),
            source,
        })?;
        let mut raw = toml::from_str::<RawConfig>(&text).map_err(|source| Error::Invalid {
            path: path.to_owned(),
            source,
        })?;
        raw.theme = raw.theme.map(|theme| theme.resolve_relative_to(path));
        Ok(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        Config::load(Some(file.path()))
    }

    #[test]
    fn loads_server_entries_and_capture_mappings_from_config() {
        const SHORTHAND_LANGUAGE: &str = "python";
        const DETAILED_LANGUAGE: &str = "go";
        const SERVER: &str = "custom-server";
        const STDIO_ARGUMENT: &str = "--stdio";
        const MULTI_WORD_ARGUMENT: &str = "multi word";
        const DETAILED_ARGUMENT: &str = "serve";
        const CONFIG_SECTION: &str = "gopls";
        const CONFIG_OPTION: &str = "semanticTokens";
        const CONFIG_LIST: &str = "analyses";
        const CONFIG_LIST_VALUE: &str = "unusedparams";
        const CONFIG_NUMBER: &str = "threshold";
        const CAPTURE: &str = "decorator";
        const MAPPING: &str = "constant";
        let config = config_from_source(&format!(
            r#"
    [servers]
    {SHORTHAND_LANGUAGE} = "{SERVER} {STDIO_ARGUMENT} '{MULTI_WORD_ARGUMENT}'"

    [servers.{DETAILED_LANGUAGE}]
    command = "{SERVER} {DETAILED_ARGUMENT}"
    config = {{ {CONFIG_SECTION} = {{ {CONFIG_OPTION} = true, {CONFIG_LIST} = ["{CONFIG_LIST_VALUE}"], {CONFIG_NUMBER} = 2.5 }} }}

    [captures]
    {CAPTURE} = "{MAPPING}"
    "#,
        ))
        .unwrap();
        let shorthand = config.commands.get(SHORTHAND_LANGUAGE).unwrap();
        let detailed = config.commands.get(DETAILED_LANGUAGE).unwrap();

        assert_eq!(
            shorthand,
            &lsp::CommandEntry::new(
                SERVER,
                &[STDIO_ARGUMENT, MULTI_WORD_ARGUMENT],
                serde_json::json!({})
            )
        );
        assert_eq!(
            detailed,
            &lsp::CommandEntry::new(
                SERVER,
                &[DETAILED_ARGUMENT],
                serde_json::json!({
                    (CONFIG_SECTION): {
                        (CONFIG_OPTION): true,
                        (CONFIG_LIST): [CONFIG_LIST_VALUE],
                        (CONFIG_NUMBER): 2.5,
                    },
                }),
            )
        );
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

        let config = Config::load(Some(&config_path)).unwrap();
        let theme = theme::load(None, None, config.theme.as_ref()).unwrap();

        assert_eq!(theme.name, THEME_NAME);
    }

    #[test]
    fn rejects_invalid_config_and_server_commands() {
        let invalid_toml = config_from_source("=").unwrap_err();
        let empty_command = config_from_source("[servers]\nrust = ' '").unwrap_err();
        let invalid_command = config_from_source("[servers]\nrust = \"'\"").unwrap_err();

        assert!(matches!(invalid_toml, Error::Invalid { .. }));
        assert!(matches!(empty_command, Error::EmptyCommand(_)));
        assert!(matches!(invalid_command, Error::InvalidCommand { .. }));
    }
}
