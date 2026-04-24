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
        // Resolve "kilometers" to "km"
        let from_unit = self.db.resolve_symbol(from_input)?;

        let entry = self.db.get_unit(&from_unit)?
            .ok_or_else(|| anyhow!("Unknown unit: {from_input}"))?;

        // Math: Base = (Input + Offset) * Factor
        let base_value = (value + entry.offset) * entry.factor;

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

        // Limit results
        if outputs.len() > self.config.list_size {
            outputs.truncate(self.config.list_size);
        }

        Ok(ConversionResult {
            input_value: value,
            input_unit: from_input.to_string(),
            outputs,
        })
    }
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
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat).unwrap();
        // USD (e.g. 1.1 USD per 1 EUR) -> factor = 1/1.1 ? 
        // Wait, if Base = (Value + Offset) * Factor, and Base is EUR.
        // For USD: Base_EUR = (Value_USD + 0) * (1/1.1)
        // So factor = 1.0 / 1.1
        db.update_unit("USD", 1.0 / 1.1, 0.0, UnitCategory::Currency, RateSource::Fiat).unwrap();
        
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
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat).unwrap();
        
        // 2. PLN (Fiat): 1 EUR = 4.0 PLN -> Factor = 1/4 = 0.25
        db.update_rate("PLN", 4.0, 1000, RateSource::Fiat).unwrap();
        
        // 3. BTC (Crypto): 1 BTC = 50000 EUR -> Factor = 50000
        db.update_rate("BTC", 50000.0, 1000, RateSource::Crypto).unwrap();
        
        let converter = Converter::new(config, db);

        // Convert 1 BTC to PLN
        // Base_EUR = 1 * 50000 = 50000
        // Target_PLN = 50000 / 0.25 = 200000
        let res = converter.convert(1.0, "BTC").unwrap();
        let pln = res.outputs.iter().find(|o| o.unit == "PLN").unwrap();
        assert!((pln.value - 200000.0).abs() < f64::EPSILON);

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
}
