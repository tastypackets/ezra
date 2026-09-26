use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub manager: ManagerSettings,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagerSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_hash: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("could not read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("{path} is not valid TOML: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("could not write {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("could not serialize the settings: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// A missing file means default settings.
pub fn load(path: &Path) -> Result<Settings, SettingsError> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|source| SettingsError::Parse {
            path: path.to_owned(),
            source,
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(source) => Err(SettingsError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

pub fn save(path: &Path, settings: &Settings) -> Result<(), SettingsError> {
    let text = toml::to_string_pretty(settings)?;
    let write = || -> io::Result<()> {
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)?;
        }
        fs::write(path, text)
    };
    write().map_err(|source| SettingsError::Write {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_means_defaults() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            load(&directory.path().join("settings.toml")).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn saved_settings_load_back() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent-box/settings.toml");
        let settings = Settings {
            manager: ManagerSettings {
                password_hash: Some("$argon2id$example".to_owned()),
            },
        };
        save(&path, &settings).unwrap();
        assert_eq!(load(&path).unwrap(), settings);
    }

    #[test]
    fn invalid_toml_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        fs::write(&path, "[manager\n").unwrap();
        assert!(matches!(load(&path), Err(SettingsError::Parse { .. })));
    }
}
