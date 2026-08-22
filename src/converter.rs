use crate::db::Db;
use crate::models::{Config, ConversionResult, ConvertedValue, UnitInfo};
use anyhow::{Result, anyhow};

/// A unit with precomputed lowercase forms so search never allocates per query.
#[derive(Debug, Clone)]
pub struct SearchableUnit {
    /// The unit's display data (symbol and aliases).
    pub info: UnitInfo,
    /// Lowercased symbol.
    symbol_lower: String,
    /// Lowercased aliases.
    aliases_lower: Vec<String>,
}

impl SearchableUnit {
    fn new(info: UnitInfo) -> Self {
        let symbol_lower = info.symbol.to_lowercase();
        let aliases_lower = info.aliases.iter().map(|a| a.to_lowercase()).collect();
        Self {
            info,
            symbol_lower,
            aliases_lower,
        }
    }

    /// Returns `true` if the lowercased query is a substring of the symbol or any alias.
    #[must_use]
    pub fn matches(&self, query_lower: &str) -> bool {
        self.symbol_lower.contains(query_lower)
            || self.aliases_lower.iter().any(|a| a.contains(query_lower))
    }

    /// Returns `true` if the lowercased query equals the symbol or any alias exactly.
    #[must_use]
    pub fn matches_exact(&self, query_lower: &str) -> bool {
        self.symbol_lower == query_lower || self.aliases_lower.iter().any(|a| a == query_lower)
    }
}

/// The core conversion engine.
pub struct Converter {
    /// User configuration for sorting and limits.
    config: Config,
    /// Database handle for currency rates and units.
    db: Db,
    /// Cached unit list to avoid repeated full-table reads during UI frames.
    units_cache: Option<Vec<SearchableUnit>>,
}

impl Converter {
    /// Creates a new `Converter` with the provided configuration and database handle.
    #[must_use]
    pub const fn new(config: Config, db: Db) -> Self {
        Self {
            config,
            db,
            units_cache: None,
        }
    }

    /// Replaces the configuration (e.g. after a favorites change) without
    /// discarding the cached unit list.
    pub fn set_config(&mut self, config: Config) {
        if self.config.unit_packs != config.unit_packs {
            self.units_cache = None;
        }
        self.config = config;
    }

    /// Clears the cached unit list so the next [`Self::all_units`] call reloads from the DB.
    ///
    /// Call this after background rate refreshes add or update symbols.
    pub fn invalidate_units_cache(&mut self) {
        self.units_cache = None;
    }

    /// Returns a borrowed slice of all supported units with their aliases and
    /// precomputed lowercase search forms, populating the internal cache on
    /// first use to avoid repeated full-table reads and per-call allocations.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn all_units(&mut self) -> Result<&[SearchableUnit]> {
        if self.units_cache.is_none() {
            let unit_map = self.db.get_all_units_with_aliases()?;
            let mut result: Vec<SearchableUnit> = unit_map
                .into_iter()
                .filter(|(_, (_, category))| self.config.unit_packs.allows(*category))
                .map(|(symbol, (aliases, category))| {
                    SearchableUnit::new(UnitInfo {
                        symbol,
                        aliases,
                        category,
                    })
                })
                .collect();
            result.sort_by(|a, b| a.info.symbol.cmp(&b.info.symbol));
            self.units_cache = Some(result);
        }
        // Cache is populated above whenever it was `None`.
        Ok(self.units_cache.as_deref().unwrap_or(&[]))
    }

    /// Converts a numeric value from one unit to all compatible target units.
    ///
    /// # Errors
    /// Returns an error if the input unit is unknown or if the conversion fails.
    pub fn convert(&self, value: f64, from_input: &str) -> Result<ConversionResult> {
        self.convert_preferring(value, from_input, None)
    }

    /// Like [`Self::convert`], but pins `prefer` at the front of the output list
    /// when that symbol exists in the same category.
    ///
    /// # Errors
    /// Returns an error if the input unit is unknown or if the conversion fails.
    pub fn convert_preferring(
        &self,
        value: f64,
        from_input: &str,
        prefer: Option<&str>,
    ) -> Result<ConversionResult> {
        let mut actual_value = value;
        let mut parsed_unit = from_input;

        // 1. Check for currency multipliers (e.g., "B USD")
        if let Some((factor, rest)) = extract_currency_multiplier(from_input) {
            let resolved_rest = self.db.resolve_symbol(rest)?;
            if let Ok(Some(entry)) = self.db.get_unit(&resolved_rest)
                && entry.category == crate::models::UnitCategory::Currency as u8
            {
                actual_value *= factor;
                parsed_unit = rest;
            }
        }

        // Resolve "kilometers" to "km"
        let mut from_unit = self.db.resolve_symbol(parsed_unit)?;

        let mut entry_opt = self.db.get_unit(&from_unit)?;

        // 2. Metric prefix fallback
        if entry_opt.is_none()
            && let Some((factor, rest)) = extract_metric_prefix(parsed_unit)
        {
            let resolved_rest = self.db.resolve_symbol(rest)?;
            if let Ok(Some(rest_entry)) = self.db.get_unit(&resolved_rest)
                && rest_entry.category != crate::models::UnitCategory::Currency as u8
            {
                actual_value *= factor;
                from_unit = resolved_rest;
                entry_opt = Some(rest_entry);
            }
        }
        let entry = entry_opt.ok_or_else(|| anyhow!("Unknown unit: {from_input}"))?;

        // Math: Base = (Input + Offset) * Factor
        let base_value = (actual_value + entry.offset) * entry.factor;

        let mut outputs = Vec::new();
        let targets = self.db.get_category_units(entry.category)?;

        for (symbol, target_entry) in targets {
            if symbol != from_unit {
                // Math: Target = (Base / Factor) - Offset
                let target_val = (base_value / target_entry.factor) - target_entry.offset;
                outputs.push(ConvertedValue {
                    value: target_val,
                    unit: symbol,
                });
            }
        }

        // Deduplicate units
        outputs.sort_by(|a, b| a.unit.cmp(&b.unit));
        outputs.dedup_by(|a, b| a.unit == b.unit);

        let ranks = crate::models::favorite_ranks(&self.config.favorites);
        outputs.sort_by(|a, b| crate::models::cmp_favorite_rank(&a.unit, &b.unit, &ranks));

        if let Some(prefer) = prefer.filter(|s| !s.is_empty()) {
            let resolved = self.db.resolve_symbol(prefer)?;
            if let Some(idx) = outputs.iter().position(|o| o.unit == resolved) {
                let preferred = outputs.remove(idx);
                outputs.insert(0, preferred);
            }
        }

        Ok(ConversionResult {
            input_value: value,
            input_unit: from_input.to_string(),
            outputs,
        })
    }
}

/// Matches an ASCII prefix table against `input`.
///
/// # Safety invariant
/// All prefixes must be pure ASCII so that `prefix.len()` is valid
/// for slicing both the lowercase and original mixed-case strings.
fn prefix_factor<'a>(input: &'a str, prefixes: &[(&str, f64)]) -> Option<(f64, &'a str)> {
    let lower = input.to_lowercase();
    for &(prefix, factor) in prefixes {
        debug_assert!(prefix.is_ascii(), "prefixes must be ASCII");
        if lower.starts_with(prefix) {
            return Some((factor, &input[prefix.len()..]));
        }
    }
    None
}

/// Extracts a currency multiplier from the start of the unit string.
fn extract_currency_multiplier(input: &str) -> Option<(f64, &str)> {
    let input = input.trim();
    prefix_factor(
        input,
        &[
            ("k ", 1e3),
            ("m ", 1e6),
            ("b ", 1e9),
            ("t ", 1e12),
            ("thousand ", 1e3),
            ("million ", 1e6),
            ("billion ", 1e9),
            ("trillion ", 1e12),
        ],
    )
    .map(|(factor, rest)| (factor, rest.trim()))
}

/// Extracts a metric prefix (e.g., "kilo", "nano") from the start of the unit string.
fn extract_metric_prefix(input: &str) -> Option<(f64, &str)> {
    prefix_factor(
        input,
        &[
            ("exa", 1e18),
            ("peta", 1e15),
            ("tera", 1e12),
            ("giga", 1e9),
            ("mega", 1e6),
            ("kilo", 1e3),
            ("hecto", 1e2),
            ("deca", 1e1),
            ("deci", 1e-1),
            ("centi", 1e-2),
            ("milli", 1e-3),
            ("micro", 1e-6),
            ("nano", 1e-9),
            ("pico", 1e-12),
            ("femto", 1e-15),
            ("atto", 1e-18),
        ],
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;
    use crate::models::{RateSource, UnitCategory};
    use redb::Database;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    /// Relative-tolerance float assertion: tolerance scales with magnitude so
    /// large conversion results don't fail on accumulated rounding error.
    fn assert_relative_eq(a: f64, b: f64) {
        let tolerance = 1e-9 * a.abs().max(b.abs()).max(1.0);
        assert!(
            (a - b).abs() <= tolerance,
            "expected {a} to equal {b} within relative tolerance"
        );
    }

    fn create_test_db() -> Db {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db::open_for_test(Arc::new(db_inner));
        db.init_static_units().unwrap();
        db
    }

    #[test]
    fn test_length_conversion() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.0, "m").unwrap();
        let cm = res.outputs.iter().find(|o| o.unit == "cm").unwrap();
        assert_relative_eq(cm.value, 100.0);
    }

    #[test]
    fn test_currency_conversion() {
        let config = Config::default();
        let db = create_test_db();
        // EUR is base (factor 1.0, offset 0.0)
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        // USD (e.g. 1.1 USD per 1 EUR) -> factor = 1/1.1
        db.update_unit(
            "USD",
            1.0 / 1.1,
            0.0,
            UnitCategory::Currency,
            RateSource::Fiat,
        )
        .unwrap();

        let converter = Converter::new(config, db);

        let res = converter.convert(10.0, "EUR").unwrap();
        let usd = res.outputs.iter().find(|o| o.unit == "USD").unwrap();
        // Target = (Base / Factor) - Offset
        // Target_USD = (10.0 / (1.0/1.1)) - 0 = 11.0
        assert_relative_eq(usd.value, 11.0);
    }

    #[test]
    fn test_temperature_conversion() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        // 0 C to F
        let res = converter.convert(0.0, "C").unwrap();
        let f = res.outputs.iter().find(|o| o.unit == "F").unwrap();
        assert_relative_eq(f.value, 32.0);

        // 32 F to C
        let res = converter.convert(32.0, "F").unwrap();
        let c = res.outputs.iter().find(|o| o.unit == "C").unwrap();
        assert_relative_eq(c.value, 0.0);
    }

    #[test]
    fn test_alias_conversion() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        // "meters" should resolve to "m"
        let res = converter.convert(1.0, "meters").unwrap();
        let cm = res.outputs.iter().find(|o| o.unit == "cm").unwrap();
        assert_relative_eq(cm.value, 100.0);
    }

    #[test]
    fn test_cross_currency_conversion() {
        let config = Config::default();
        let db = create_test_db();

        // 1. EUR is base
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();

        // 2. PLN (Fiat): 1 EUR = 4.0 PLN -> Factor = 1/4 = 0.25
        db.update_rate("PLN", 4.0, 1000, RateSource::Fiat).unwrap();

        // 3. BTC (Crypto): 1 BTC = 50000 EUR -> Factor = 50000
        db.update_rate("BTC", 50000.0, 1000, RateSource::Crypto)
            .unwrap();

        let converter = Converter::new(config, db);

        // Convert 1 BTC to PLN
        // Base_EUR = 1 * 50000 = 50000
        // Target_PLN = 50000 / 0.25 = 200000
        let res = converter.convert(1.0, "BTC").unwrap();
        let pln = res.outputs.iter().find(|o| o.unit == "PLN").unwrap();
        assert_relative_eq(pln.value, 200_000.0);

        // Convert 4 PLN to BTC
        // Base_EUR = 4 * 0.25 = 1.0
        // Target_BTC = 1.0 / 50000 = 0.00002
        let res = converter.convert(4.0, "PLN").unwrap();
        let btc = res.outputs.iter().find(|o| o.unit == "BTC").unwrap();
        assert_relative_eq(btc.value, 0.00002);
    }

    #[test]
    fn test_deduplication_and_sorting() {
        let config = Config {
            favorites: vec!["ft".to_string()],
            ..Config::default()
        };
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.0, "m").unwrap();

        // Ensure "m" is not present in outputs when it's the input
        let m_count = res.outputs.iter().filter(|o| o.unit == "m").count();
        assert_eq!(m_count, 0, "Input unit should not be in output");

        // "ft" should be first because it's a favorite
        assert_eq!(res.outputs[0].unit, "ft");
    }

    #[test]
    fn test_currency_multipliers() {
        let config = Config::default();
        let db = create_test_db();
        db.update_unit("USD", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        db.update_unit("EUR", 0.9, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.5, "B USD").unwrap();
        assert_eq!(res.input_value, 1.5);
        assert_eq!(res.input_unit, "B USD");

        let eur = res.outputs.iter().find(|o| o.unit == "EUR").unwrap();
        // 1.5B USD -> 1,500,000,000 USD.
        // 1 USD = 1 Base
        // Base = 1.5e9.
        // EUR factor = 0.9.
        // Target_EUR = (1.5e9 / 0.9) = 1,666,666,666.66...
        assert!((eur.value - 1_666_666_666.6).abs() < 1.0);
    }

    #[test]
    fn test_metric_prefixes_fallback() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        // Convert 1 nanometer to cm
        // 1 nanometers -> actual_value = 1e-9, from_unit = "m"
        // 1e-9 m to cm -> Target = (1e-9 / 0.01) = 1e-7
        let res = converter.convert(1.0, "nanometers").unwrap();
        let cm = res.outputs.iter().find(|o| o.unit == "cm").unwrap();
        assert!((cm.value - 1e-7).abs() < 1e-10);
    }

    #[test]
    fn convert_preferring_should_pin_requested_unit_first() {
        let config = Config::default();
        let db = create_test_db();
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        db.update_unit(
            "USD",
            1.0 / 1.1,
            0.0,
            UnitCategory::Currency,
            RateSource::Fiat,
        )
        .unwrap();
        db.update_unit("PLN", 0.25, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        let converter = Converter::new(config, db);

        let res = converter
            .convert_preferring(10.0, "EUR", Some("PLN"))
            .unwrap();
        assert_eq!(res.outputs[0].unit, "PLN");
        assert_relative_eq(res.outputs[0].value, 40.0);
        assert!(res.outputs.iter().any(|o| o.unit == "USD"));
    }

    #[test]
    fn all_units_should_honor_disabled_packs() {
        let mut config = Config::default();
        config.unit_packs.volume = false;
        config.unit_packs.scientific = false;
        let db = create_test_db();
        let mut converter = Converter::new(config, db);

        let symbols: Vec<&str> = converter
            .all_units()
            .unwrap()
            .iter()
            .map(|u| u.info.symbol.as_str())
            .collect();
        assert!(!symbols.contains(&"L"));
        assert!(!symbols.contains(&"Pa"));
        assert!(symbols.contains(&"m"));
    }

    #[test]
    fn convert_should_still_work_for_disabled_pack_on_capture() {
        let mut config = Config::default();
        config.unit_packs.volume = false;
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.0, "L").unwrap();
        assert!(res.outputs.iter().any(|o| o.unit == "gal"));
    }

    #[test]
    fn convert_should_handle_volume_and_speed() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.0, "L").unwrap();
        let gal = res.outputs.iter().find(|o| o.unit == "gal").unwrap();
        assert!((gal.value - 1.0 / 3.785_411_784).abs() < 1e-9);

        let res = converter.convert(36.0, "km/h").unwrap();
        let mps = res.outputs.iter().find(|o| o.unit == "m/s").unwrap();
        assert!((mps.value - 10.0).abs() < 1e-9);
    }

    #[test]
    fn convert_should_handle_scientific_pressure() {
        let mut config = Config::default();
        config.unit_packs.scientific = true;
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.0, "bar").unwrap();
        let pa = res.outputs.iter().find(|o| o.unit == "Pa").unwrap();
        assert!((pa.value - 100_000.0).abs() < 1e-6);
    }
}
