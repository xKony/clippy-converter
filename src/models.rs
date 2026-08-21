use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{error, warn};

use crate::hotkey;

/// Default global hotkey combination.
pub const DEFAULT_HOTKEY: &str = "Shift+Alt+C";
/// Minimum allowed rate-refresh interval in minutes.
pub const MIN_UPDATE_INTERVAL_MINS: u64 = 5;
/// Maximum allowed rate-refresh interval in minutes (one week).
pub const MAX_UPDATE_INTERVAL_MINS: u64 = 10_080;
/// Default fiat refresh interval in minutes.
pub const DEFAULT_FIAT_INTERVAL_MINS: u64 = 1440;
/// Default crypto refresh interval in minutes.
pub const DEFAULT_CRYPTO_INTERVAL_MINS: u64 = 60;

/// Configuration for the Clippy Converter application.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    /// List of user-favorite units for quick access.
    pub favorites: Vec<String>,
    /// Global hotkey combination (e.g., "Shift+Alt+C").
    pub hotkey: String,
    /// When true, the hotkey copies the current selection before opening the popup.
    #[serde(default = "default_read_selection_on_hotkey")]
    pub read_selection_on_hotkey: bool,
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
    /// How to group thousands in displayed numbers (not used when copying).
    #[serde(default)]
    pub thousand_separator: ThousandSeparator,
    /// Optional extra unit groups. Core length/weight/temperature/time/currency stay on.
    #[serde(default)]
    pub unit_packs: UnitPacks,
    /// Write this executable into the current-user Windows Run key.
    #[serde(default)]
    pub start_with_windows: bool,
}

/// Visual grouping of digits in displayed numbers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThousandSeparator {
    /// No grouping (e.g. `1234567.89`).
    #[default]
    None,
    /// Space-separated (e.g. `1 234 567.89`).
    Space,
    /// Comma-separated (e.g. `1,234,567.89`).
    Comma,
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

/// Favorite-rank lookup built once so sorts don't scan the list per comparison.
#[must_use]
pub(crate) fn favorite_ranks(favorites: &[String]) -> HashMap<&str, usize> {
    favorites
        .iter()
        .enumerate()
        .map(|(idx, fav)| (fav.as_str(), idx))
        .collect()
}

#[must_use]
pub(crate) fn cmp_favorite_rank(a: &str, b: &str, ranks: &HashMap<&str, usize>) -> Ordering {
    match (ranks.get(a), ranks.get(b)) {
        (Some(ai), Some(bi)) => ai.cmp(bi),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

const fn default_read_selection_on_hotkey() -> bool {
    true
}

const fn default_true() -> bool {
    true
}

/// Returns `mins` when inside `MIN_UPDATE_INTERVAL_MINS..=MAX_UPDATE_INTERVAL_MINS`,
/// otherwise warns and returns `default`.
#[must_use]
pub fn sanitize_interval_mins(field: &'static str, mins: u64, default: u64) -> u64 {
    if (MIN_UPDATE_INTERVAL_MINS..=MAX_UPDATE_INTERVAL_MINS).contains(&mins) {
        mins
    } else {
        warn!(
            field,
            mins,
            min = MIN_UPDATE_INTERVAL_MINS,
            max = MAX_UPDATE_INTERVAL_MINS,
            "refresh interval out of range; falling back to default"
        );
        default
    }
}

/// Optional unit groups the user can toggle in Settings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each pack is an independent settings checkbox"
)]
pub struct UnitPacks {
    /// Litres, gallons, fluid ounces.
    #[serde(default = "default_true")]
    pub volume: bool,
    /// Square metres, acres, hectares.
    #[serde(default = "default_true")]
    pub area: bool,
    /// km/h, mph, knots.
    #[serde(default = "default_true")]
    pub speed: bool,
    /// Bytes, KB/KiB, and larger.
    #[serde(default = "default_true")]
    pub data: bool,
    /// Pressure, energy, power, force, angle, frequency.
    #[serde(default)]
    pub scientific: bool,
}

impl Default for UnitPacks {
    fn default() -> Self {
        Self {
            volume: true,
            area: true,
            speed: true,
            data: true,
            scientific: false,
        }
    }
}

impl UnitPacks {
    /// Core categories are always listed; extra packs follow these flags.
    #[must_use]
    pub const fn allows(self, category: u8) -> bool {
        if category == UnitCategory::Volume as u8 {
            return self.volume;
        }
        if category == UnitCategory::Area as u8 {
            return self.area;
        }
        if category == UnitCategory::Speed as u8 {
            return self.speed;
        }
        if category == UnitCategory::Data as u8 {
            return self.data;
        }
        if category == UnitCategory::Pressure as u8
            || category == UnitCategory::Energy as u8
            || category == UnitCategory::Power as u8
            || category == UnitCategory::Force as u8
            || category == UnitCategory::Angle as u8
            || category == UnitCategory::Frequency as u8
        {
            return self.scientific;
        }
        true
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
            hotkey: DEFAULT_HOTKEY.to_string(),
            read_selection_on_hotkey: true,
            list_size: 10,
            history_enabled: false,
            history_retention: HistoryRetention::ThirtyDays,
            fiat_update_interval_mins: DEFAULT_FIAT_INTERVAL_MINS, // Daily
            crypto_update_interval_mins: DEFAULT_CRYPTO_INTERVAL_MINS, // Every hour
            thousand_separator: ThousandSeparator::None,
            unit_packs: UnitPacks::default(),
            start_with_windows: false,
        }
    }
}

impl Config {
    /// Loads the configuration from the user's config directory.
    ///
    /// Invalid or corrupt fields are salvaged: out-of-range values and unparseable
    /// hotkeys fall back to their defaults with a warning, and a wholly corrupt
    /// file falls back to [`Config::default`] instead of failing startup.
    ///
    /// # Errors
    /// Returns an error only if the config directory cannot be determined or the
    /// existing file cannot be read (parse failures are salvaged, not propagated).
    pub fn load() -> Result<Self> {
        let path = get_config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        Self::load_from_path(&path)
    }

    /// Loads the configuration from an explicit path. Split out so tests can use
    /// temp files instead of the real user config directory.
    fn load_from_path(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file at {}", path.display()))?;
        Ok(Self::from_json(&content))
    }

    /// Parses config JSON, salvaging per-field problems:
    /// a parse failure logs an error and returns defaults; individual out-of-range
    /// or invalid fields are replaced by their defaults with a warning.
    #[must_use]
    pub fn from_json(content: &str) -> Self {
        match serde_json::from_str::<Self>(content) {
            Ok(config) => config.sanitized(),
            Err(err) => {
                error!(error = %err, "failed to parse config JSON; using default config");
                Self::default()
            }
        }
    }

    /// Returns a copy with any invalid field replaced by its default (with a warning).
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.fiat_update_interval_mins = sanitize_interval_mins(
            "fiat_update_interval_mins",
            self.fiat_update_interval_mins,
            DEFAULT_FIAT_INTERVAL_MINS,
        );
        self.crypto_update_interval_mins = sanitize_interval_mins(
            "crypto_update_interval_mins",
            self.crypto_update_interval_mins,
            DEFAULT_CRYPTO_INTERVAL_MINS,
        );
        if hotkey::parse_hotkey(&self.hotkey).is_err() {
            warn!(hotkey = %self.hotkey, "invalid hotkey in config; falling back to default");
            self.hotkey = DEFAULT_HOTKEY.to_string();
        }
        self
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

/// Represents the source of a currency rate or unit.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum RateSource {
    /// Daily fallback from fiat API.
    Fiat = 0,
    /// High-frequency update from crypto API.
    Crypto = 1,
    /// Static baked-in unit.
    Static = 2,
}

/// Categories for compatible groups of units.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum UnitCategory {
    /// Monetary units (e.g., USD, EUR).
    Currency = 0,
    /// Linear measurements (e.g., m, ft).
    Length = 1,
    /// Mass measurements (e.g., kg, lb).
    Weight = 2,
    /// Thermal measurements (e.g., C, F, K).
    Temperature = 3,
    /// Time measurements (e.g., s, ms).
    Time = 4,
    /// Capacity (e.g., L, gal).
    Volume = 5,
    /// Surface (e.g., m2, acre).
    Area = 6,
    /// Velocity (e.g., km/h, mph).
    Speed = 7,
    /// Digital size (e.g., B, MiB).
    Data = 8,
    /// Pressure (e.g., Pa, psi).
    Pressure = 9,
    /// Energy (e.g., J, kWh).
    Energy = 10,
    /// Power (e.g., W, hp).
    Power = 11,
    /// Force (e.g., N, lbf).
    Force = 12,
    /// Angle (e.g., deg, rad).
    Angle = 13,
    /// Frequency (e.g., Hz, kHz).
    Frequency = 14,
}

/// A unified rate/unit entry stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnitEntry {
    /// Factor to multiply the base value.
    pub factor: f64,
    /// Offset to add to the value before multiplying.
    pub offset: f64,
    /// Unit category (matches `UnitCategory`).
    pub category: u8,
    /// Timestamp of last update.
    pub timestamp: i64,
    /// Source of the rate.
    pub source: u8,
}

/// Structured unit information for the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnitInfo {
    /// Canonical symbol (e.g., "m").
    pub symbol: String,
    /// List of aliases (e.g., `["meter", "meters"]`).
    pub aliases: Vec<String>,
    /// Unit category (matches `UnitCategory`).
    pub category: u8,
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

    #[test]
    fn test_config_missing_read_selection_defaults_true() {
        let json = r#"{
            "favorites": [],
            "hotkey": "Shift+Alt+C",
            "list_size": 10,
            "history_enabled": false,
            "history_retention": "ThirtyDays",
            "fiat_update_interval_mins": 1440,
            "crypto_update_interval_mins": 60
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.read_selection_on_hotkey);
    }

    #[test]
    fn test_config_default_reads_selection() {
        assert!(Config::default().read_selection_on_hotkey);
    }

    #[test]
    fn test_config_missing_unit_packs_defaults_everyday_on() {
        let json = r#"{
            "favorites": [],
            "hotkey": "Shift+Alt+C",
            "list_size": 10,
            "history_enabled": false,
            "history_retention": "ThirtyDays",
            "fiat_update_interval_mins": 1440,
            "crypto_update_interval_mins": 60
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.unit_packs.volume);
        assert!(!config.unit_packs.scientific);
        assert!(!config.start_with_windows);
    }

    #[test]
    fn sanitized_should_salvage_fiat_interval_below_minimum_to_default() {
        let config = Config {
            fiat_update_interval_mins: MIN_UPDATE_INTERVAL_MINS - 1,
            ..Config::default()
        };
        let sanitized = config.sanitized();
        assert_eq!(sanitized.fiat_update_interval_mins, DEFAULT_FIAT_INTERVAL_MINS);
        assert_eq!(
            sanitized.crypto_update_interval_mins,
            DEFAULT_CRYPTO_INTERVAL_MINS
        );
    }

    #[test]
    fn sanitized_should_salvage_crypto_interval_above_maximum_to_default() {
        let config = Config {
            crypto_update_interval_mins: MAX_UPDATE_INTERVAL_MINS + 1,
            ..Config::default()
        };
        let sanitized = config.sanitized();
        assert_eq!(
            sanitized.crypto_update_interval_mins,
            DEFAULT_CRYPTO_INTERVAL_MINS
        );
        assert_eq!(sanitized.fiat_update_interval_mins, DEFAULT_FIAT_INTERVAL_MINS);
    }

    #[test]
    fn sanitized_should_keep_intervals_at_inclusive_bounds() {
        let config = Config {
            fiat_update_interval_mins: MIN_UPDATE_INTERVAL_MINS,
            crypto_update_interval_mins: MAX_UPDATE_INTERVAL_MINS,
            ..Config::default()
        };
        let sanitized = config.sanitized();
        assert_eq!(sanitized.fiat_update_interval_mins, MIN_UPDATE_INTERVAL_MINS);
        assert_eq!(
            sanitized.crypto_update_interval_mins,
            MAX_UPDATE_INTERVAL_MINS
        );
    }

    #[test]
    fn sanitized_should_salvage_invalid_hotkey_string_to_default() {
        for bad in ["", "NotAKey", "Shift+", "Shift+Alt+C+D", "   "] {
            let config = Config {
                hotkey: bad.to_string(),
                ..Config::default()
            };
            let sanitized = config.sanitized();
            assert_eq!(sanitized.hotkey, DEFAULT_HOTKEY, "input was {bad:?}");
        }
    }

    #[test]
    fn sanitized_should_keep_valid_hotkey_string_untouched() {
        let config = Config {
            hotkey: "Ctrl+Space".to_string(),
            ..Config::default()
        };
        assert_eq!(config.sanitized().hotkey, "Ctrl+Space");
    }

    #[test]
    fn from_json_should_fall_back_to_defaults_for_wholly_corrupt_json() {
        let config = Config::from_json("{not valid json!!");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn from_json_should_salvage_valid_fields_alongside_invalid_ones() {
        let json = format!(
            r#"{{
                "favorites": ["USD"],
                "hotkey": "Bogus",
                "list_size": 10,
                "history_enabled": false,
                "history_retention": "ThirtyDays",
                "fiat_update_interval_mins": {},
                "crypto_update_interval_mins": {}
            }}"#,
            MIN_UPDATE_INTERVAL_MINS - 1,
            MAX_UPDATE_INTERVAL_MINS + 1
        );
        let config = Config::from_json(&json);
        assert_eq!(config.favorites, vec!["USD".to_string()]);
        assert_eq!(config.hotkey, DEFAULT_HOTKEY);
        assert_eq!(config.fiat_update_interval_mins, DEFAULT_FIAT_INTERVAL_MINS);
        assert_eq!(
            config.crypto_update_interval_mins,
            DEFAULT_CRYPTO_INTERVAL_MINS
        );
    }

    #[test]
    fn load_from_path_should_preserve_valid_config_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let original = Config {
            fiat_update_interval_mins: MIN_UPDATE_INTERVAL_MINS,
            crypto_update_interval_mins: MAX_UPDATE_INTERVAL_MINS,
            hotkey: "Ctrl+Space".to_string(),
            ..Config::default()
        };
        fs::write(&path, serde_json::to_string(&original).unwrap()).unwrap();

        let loaded = Config::load_from_path(&path).unwrap();
        assert_eq!(loaded, original);
    }

    #[test]
    fn load_from_path_should_fall_back_to_defaults_for_corrupt_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "{{{ definitely not JSON").unwrap();

        let loaded = Config::load_from_path(&path).unwrap();
        assert_eq!(loaded, Config::default());
    }
}
