use std::fs;
use std::path::{Path, PathBuf};

use arborium::theme::Theme;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Config {
    Builtin(String),
    Custom { path: PathBuf },
}

impl Config {
    pub fn from_options(builtin: Option<&str>, custom: Option<&Path>) -> Result<Option<Self>> {
        match (builtin, custom) {
            (Some(name), None) => Ok(Some(Self::Builtin(name.to_owned()))),
            (None, Some(path)) => fs::canonicalize(path)
                .map(|path| Some(Self::Custom { path }))
                .map_err(|source| Error::Read {
                    path: path.to_owned(),
                    source,
                }),
            (None, None) => Ok(None),
            (Some(_), Some(_)) => unreachable!("CLI rejects conflicting theme options"),
        }
    }

    pub(crate) fn resolve_relative_to(self, config_path: &Path) -> Self {
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

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to read theme file '{}'", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Invalid theme file '{}'", .path.display())]
    Invalid {
        path: PathBuf,
        #[source]
        source: arborium_theme::ThemeError,
    },
    #[error("Unknown built-in theme '{0}'")]
    UnknownBuiltin(String),
}

type Result<T> = std::result::Result<T, Error>;

pub fn load(
    builtin: Option<&str>,
    custom: Option<&Path>,
    configured: Option<&Config>,
) -> Result<Theme> {
    match Config::from_options(builtin, custom)?
        .as_ref()
        .or(configured)
    {
        Some(Config::Builtin(name)) => load_builtin(name),
        Some(Config::Custom { path }) => load_custom(path),
        None => Ok(default()),
    }
}

fn load_custom(path: &Path) -> Result<Theme> {
    let text = fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_owned(),
        source,
    })?;
    Theme::from_toml(&text).map_err(|source| Error::Invalid {
        path: path.to_owned(),
        source,
    })
}

fn load_builtin(name: &str) -> Result<Theme> {
    arborium::theme::builtin::all()
        .into_iter()
        .find(|theme| theme.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| Error::UnknownBuiltin(name.to_owned()))
}

pub(crate) fn default() -> Theme {
    arborium_theme::builtin::catppuccin_mocha()
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVALID_THEME: &str = "=";
    const THEME_FILE: &str = "theme.toml";
    const UNKNOWN_THEME: &str = "unknown-theme";

    #[test]
    fn reports_theme_errors_from_the_theme_module() {
        let directory = tempfile::tempdir().unwrap();
        let missing_path = directory.path().join(THEME_FILE);
        let read_error = load(None, Some(&missing_path), None).unwrap_err();

        let file = tempfile::Builder::new()
            .suffix(THEME_FILE)
            .tempfile()
            .unwrap();
        fs::write(file.path(), INVALID_THEME).unwrap();
        let invalid_error = load(None, Some(file.path()), None).unwrap_err();

        let unknown_error = load(Some(UNKNOWN_THEME), None, None).unwrap_err();

        assert!(matches!(read_error, Error::Read { .. }));
        assert!(matches!(invalid_error, Error::Invalid { .. }));
        assert!(matches!(unknown_error, Error::UnknownBuiltin(_)));
    }
}
