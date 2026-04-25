use crate::db::Db;
use crate::models::{Config, ConversionResult, ConvertedValue, UnitInfo};
use anyhow::{Result, anyhow};

/// The core conversion engine.
pub struct Converter {
    /// User configuration for sorting and limits.
    config: Config,
    /// Database handle for currency rates and units.
    db: Db,
}

impl Converter {
    /// Creates a new `Converter` with the provided configuration and database handle.
    #[must_use]
    pub const fn new(config: Config, db: Db) -> Self {
        Self { config, db }
    }

    /// Returns a list of all supported units with their aliases.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_all_units(&self) -> Result<Vec<UnitInfo>> {
        let unit_map = self.db.get_all_units_with_aliases()?;
        let mut result: Vec<UnitInfo> = unit_map
            .into_iter()
            .map(|(symbol, aliases)| UnitInfo { symbol, aliases })
            .collect();
        result.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        Ok(result)
    }

    /// Converts a numeric value from one unit to all compatible target units.
    ///
    /// # Errors
    /// Returns an error if the input unit is unknown or if the conversion fails.
    pub fn convert(&self, value: f64, from_input: &str) -> Result<ConversionResult> {
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

        // Sorting logic: favorites first
        outputs.sort_by(|a, b| {
            let a_fav = self.config.favorites.iter().position(|u| u == &a.unit);
            let b_fav = self.config.favorites.iter().position(|u| u == &b.unit);

            match (a_fav, b_fav) {
                (Some(ai), Some(bi)) => ai.cmp(&bi),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.unit.cmp(&b.unit),
            }
        });

        Ok(ConversionResult {
            input_value: value,
            input_unit: from_input.to_string(),
            outputs,
        })
    }
}

/// Extracts a currency multiplier from the start of the unit string.
fn extract_currency_multiplier(input: &str) -> Option<(f64, &str)> {
    let input = input.trim();
    let multipliers = [
        ("k ", 1e3),
        ("m ", 1e6),
        ("b ", 1e9),
        ("t ", 1e12),
        ("thousand ", 1e3),
        ("million ", 1e6),
        ("billion ", 1e9),
        ("trillion ", 1e12),
    ];
    let lower = input.to_lowercase();
    for (prefix, factor) in multipliers {
        if lower.starts_with(prefix) {
            return Some((factor, input[prefix.len()..].trim()));
        }
    }
    None
}

/// Extracts a metric prefix (e.g., "kilo", "nano") from the start of the unit string.
fn extract_metric_prefix(input: &str) -> Option<(f64, &str)> {
    let lower = input.to_lowercase();
    let prefixes = [
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
    ];
    for (prefix, factor) in prefixes {
        if lower.starts_with(prefix) {
            return Some((factor, &input[prefix.len()..]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;
    use crate::models::{RateSource, UnitCategory};
    use redb::Database;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

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
        assert!((cm.value - 100.0).abs() < f64::EPSILON);
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
        assert!((usd.value - 11.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_temperature_conversion() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        // 0 C to F
        let res = converter.convert(0.0, "C").unwrap();
        let f = res.outputs.iter().find(|o| o.unit == "F").unwrap();
        assert!((f.value - 32.0).abs() < f64::EPSILON);

        // 32 F to C
        let res = converter.convert(32.0, "F").unwrap();
        let c = res.outputs.iter().find(|o| o.unit == "C").unwrap();
        assert!((c.value - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_alias_conversion() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        // "meters" should resolve to "m"
        let res = converter.convert(1.0, "meters").unwrap();
        let cm = res.outputs.iter().find(|o| o.unit == "cm").unwrap();
        assert!((cm.value - 100.0).abs() < f64::EPSILON);
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
        assert!((pln.value - 200_000.0).abs() < f64::EPSILON);

        // Convert 4 PLN to BTC
        // Base_EUR = 4 * 0.25 = 1.0
        // Target_BTC = 1.0 / 50000 = 0.00002
        let res = converter.convert(4.0, "PLN").unwrap();
        let btc = res.outputs.iter().find(|o| o.unit == "BTC").unwrap();
        assert!((btc.value - 0.00002).abs() < f64::EPSILON);
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
        db.update_unit("USD", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat).unwrap();
        db.update_unit("EUR", 0.9, 0.0, UnitCategory::Currency, RateSource::Fiat).unwrap();
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
}
