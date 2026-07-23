use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{Error, Result};

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
pub struct Config {
    pub commands: lighter::lsp::Commands,
    pub general_mapping: lighter::lsp::CaptureMapping,
    pub lang_mapping: lighter::lsp::LangCaptureMapping,
    pub theme: arborium::theme::Theme,
}

impl Config {
    pub fn load(config_path: Option<&Path>) -> Result<Self> {
        let raw = config_path
            .as_deref()
            .map(Self::read)
            .transpose()?
            .unwrap_or_default();
        let RawConfig {
            servers,
            captures,
            theme: config_theme,
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
        let theme = config_theme
            .as_ref()
            .map(|theme_config| match theme_config {
                ThemeConfig::Builtin(name) => theme::load_builtin(name),
                ThemeConfig::Custom { path } => theme::load_custom(path),
            })
            .transpose()?
            .unwrap_or_else(|| theme::default());
        Ok(Self {
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

    pub fn override_theme(self, builtin: Option<&str>, custom: Option<&Path>) -> Result<Self> {
        let theme = theme::resolve(builtin, custom, self.theme)?;
        Ok(Self { theme, ..self })
    }
}

mod theme {
    use super::{Error, Result};
    use std::fs;
    use std::path::Path;

    pub fn load_custom(path: &Path) -> Result<arborium::theme::Theme> {
        let text = fs::read_to_string(path).map_err(|source| Error::ThemeRead {
            path: path.to_owned(),
            source,
        })?;
        arborium::theme::Theme::from_toml(&text).map_err(|source| Error::InvalidTheme {
            path: path.to_owned(),
            source,
        })
    }

    pub fn load_builtin(name: &str) -> Result<arborium::theme::Theme> {
        arborium::theme::builtin::all()
            .into_iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::UnknownBuiltinTheme(name.to_owned()))
    }

    pub fn default() -> arborium::theme::Theme {
        arborium_theme::builtin::catppuccin_mocha()
    }

    pub fn resolve(
        builtin_theme: Option<&str>,
        custom_path: Option<&Path>,
        config_theme: arborium::theme::Theme,
    ) -> Result<arborium::theme::Theme> {
        match (builtin_theme, custom_path) {
            (Some(name), None) => load_builtin(name),
            (None, Some(path)) => load_custom(path),
            (None, None) => Ok(config_theme),
            (Some(_), Some(_)) => unreachable!("CLI rejects conflicting theme options"),
        }
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

        let config = Config::load(Some(&config_path)).unwrap();

        assert_eq!(config.theme.name, THEME_NAME);
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
}
