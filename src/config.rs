//! Config file: remembers which audio endpoint to pin to.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const APP_DIR_NAME: &str = "balance-enforcer";
const CONFIG_FILE_NAME: &str = "config.toml";
const LOG_DIR_NAME: &str = "logs";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unable to determine application data directory")]
    NoDataDir,

    #[error("config file not found at {0:?}; run `--install` first")]
    NotFound(PathBuf),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("failed to serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Windows endpoint ID of the device we are enforcing balance on.
    pub target_device_id: String,
    /// Friendly device name (informational; used only in logs and `--status`).
    pub target_device_name: String,
}

/// Returns the application's data directory, creating it if necessary.
pub fn app_dir() -> Result<PathBuf, ConfigError> {
    let base = dirs::config_dir().ok_or(ConfigError::NoDataDir)?;
    let app = base.join(APP_DIR_NAME);
    fs::create_dir_all(&app)?;
    Ok(app)
}

pub fn config_path() -> Result<PathBuf, ConfigError> {
    Ok(app_dir()?.join(CONFIG_FILE_NAME))
}

pub fn log_dir() -> Result<PathBuf, ConfigError> {
    let dir = app_dir()?.join(LOG_DIR_NAME);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        let path = config_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let contents = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(ConfigError::NotFound(path.to_path_buf()));
            }
            Err(e) => return Err(e.into()),
        };
        Ok(toml::from_str(&contents)?)
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        let path = config_path()?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let toml = toml::to_string_pretty(self)?;
        fs::write(path, toml)?;
        Ok(())
    }
}
