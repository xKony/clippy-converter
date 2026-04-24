use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration for the Clippy Converter application.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    /// List of user-favorite units for quick access.
    pub favorites: Vec<String>,
    /// Global hotkey combination (e.g., "Shift+Alt+C").
    pub hotkey: String,
    /// Maximum number of conversion results to show.
    pub list_size: usize,
    /// Whether to log conversions to a file.
    pub history_enabled: bool,
    /// How long to keep history logs.
    pub history_retention: HistoryRetention,
    /// Interval for refreshing fiat currency rates in minutes.
    pub fiat_update_interval_mins: u64,
    /// Interval for refreshing cryptocurrency rates in minutes.
    pub crypto_update_interval_mins: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum HistoryRetention {
    SevenDays,
    ThirtyDays,
    OneYear,
    #[default]
    Never,
}

impl HistoryRetention {
    /// Returns the number of days for retention, or None if Never.
    #[must_use]
    pub const fn to_days(self) -> Option<i64> {
        match self {
            Self::SevenDays => Some(7),
            Self::ThirtyDays => Some(30),
            Self::OneYear => Some(365),
            Self::Never => None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            favorites: vec![
                "USD".to_string(),
                "EUR".to_string(),
                "kg".to_string(),
                "lb".to_string(),
            ],
            hotkey: "Shift+Alt+C".to_string(),
            list_size: 10,
            history_enabled: false,
            history_retention: HistoryRetention::Never,
            fiat_update_interval_mins: 1440, // Daily
            crypto_update_interval_mins: 60, // Every hour
        }
    }
}

impl Config {
    /// Loads the configuration from the user's config directory.
    ///
    /// # Errors
    /// Returns an error if the config directory cannot be determined or if the file exists but is invalid.
    pub fn load() -> Result<Self> {
        let path = get_config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file at {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse config file at {}", path.display()))
    }

    /// Saves the configuration to the user's config directory.
    ///
    /// # Errors
    /// Returns an error if the config directory cannot be created or if the file cannot be written.
    pub fn save(&self) -> Result<()> {
        let path = get_config_path()?;
        save_json(&path, self)
    }
}

/// Represents a single conversion result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvertedValue {
    /// The numeric value.
    pub value: f64,
    /// The unit symbol (e.g., "kg").
    pub unit: String,
}

/// Data passed to the UI after successful parsing and conversion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionResult {
    /// The original numeric value parsed from the clipboard.
    pub input_value: f64,
    /// The original unit parsed from the clipboard.
    pub input_unit: String,
    /// All available conversion outputs.
    pub outputs: Vec<ConvertedValue>,
}

/// Helper to save a serializable value as pretty JSON to a file.
///
/// # Errors
/// Returns an error if the parent directory cannot be created or if the file cannot be written.
fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory at {}", parent.display()))?;
    }

    let content =
        serde_json::to_string_pretty(value).context("Failed to serialize data to JSON")?;
    fs::write(path, content)
        .with_context(|| format!("Failed to write data to file at {}", path.display()))
}

/// Helper to get the path to the configuration file.
fn get_config_path() -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "clippy", "clippy-converter")
        .context("Could not determine application config directory")?;
    Ok(proj_dirs.config_dir().join("config.json"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert_eq!(config.list_size, 10);
        assert!(config.favorites.contains(&"USD".to_string()));
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let json = serde_json::to_string(&config).unwrap();
        let decoded: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config, decoded);
    }
}
