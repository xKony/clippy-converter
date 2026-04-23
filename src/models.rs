use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    /// Interval for refreshing fiat currency rates in minutes.
    pub fiat_update_interval_mins: u64,
    /// Interval for refreshing cryptocurrency rates in minutes.
    pub crypto_update_interval_mins: u64,
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
            fiat_update_interval_mins: 1440, // Daily
            crypto_update_interval_mins: 1, // Every minute
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

/// Cached currency rates and metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cache {
    /// Mapping of currency codes to their rates (relative to a base).
    pub rates: HashMap<String, f64>,
    /// Timestamp of the last successful update.
    pub last_updated: DateTime<Utc>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            rates: HashMap::new(),
            last_updated: DateTime::from_timestamp(0, 0).unwrap_or_else(Utc::now),
        }
    }
}

impl Cache {
    /// Loads the cache from the user's cache directory.
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be determined or if the file exists but is invalid.
    pub fn load() -> Result<Self> {
        let path = get_cache_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read cache file at {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse cache file at {}", path.display()))
    }

    /// Saves the cache to the user's cache directory.
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be created or if the file cannot be written.  
    pub fn save(&self) -> Result<()> {
        let path = get_cache_path()?;
        save_json(&path, self)
    }

    /// Returns true if the cache is older than the configured refresh intervals or empty.
    #[must_use]
    pub fn is_expired(&self, config: &Config) -> bool {
        if self.rates.is_empty() {
            return true;
        }
        let now = Utc::now();
        let duration = now.signed_duration_since(self.last_updated);
        
        let min_interval = config.fiat_update_interval_mins.min(config.crypto_update_interval_mins);
        let min_interval_i64: i64 = min_interval.try_into().unwrap_or(i64::MAX);
        duration.num_minutes() >= min_interval_i64
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
    let proj_dirs = ProjectDirs::from("com", "konyy", "clippy-converter")
        .context("Could not determine application config directory")?;
    Ok(proj_dirs.config_dir().join("config.json"))
}

/// Helper to get the path to the cache file.
fn get_cache_path() -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "konyy", "clippy-converter")
        .context("Could not determine application cache directory")?;
    Ok(proj_dirs.cache_dir().join("cache.json"))
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn test_cache_serialization() {
        let mut cache = Cache::default();
        cache.rates.insert("PLN".to_string(), 4.5);
        let json = serde_json::to_string(&cache).unwrap();
        let decoded: Cache = serde_json::from_str(&json).unwrap();
        assert_eq!(cache, decoded);
    }

    #[test]
    fn test_cache_is_expired() {
        let mut cache = Cache::default();
        let config = Config::default();
        assert!(cache.is_expired(&config), "Default/empty cache should be expired");

        cache.rates.insert("USD".to_string(), 1.0);
        cache.last_updated = Utc::now();
        assert!(!cache.is_expired(&config), "Fresh cache should not be expired");

        // Use a value large enough to exceed default intervals
        cache.last_updated = Utc::now() - chrono::Duration::hours(25);
        assert!(cache.is_expired(&config), "Old cache should be expired");
    }
}
