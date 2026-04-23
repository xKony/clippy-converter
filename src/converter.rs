use crate::models::{Cache, Config, ConversionResult, ConvertedValue};
use anyhow::{Result, anyhow};
use std::collections::HashMap;

/// Categories for compatible groups of units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitCategory {
    /// Monetary units (e.g., USD, EUR).
    Currency,
    /// Linear measurements (e.g., m, ft).
    Length,
    /// Mass measurements (e.g., kg, lb).
    Weight,
    /// Thermal measurements (e.g., C, F, K).
    Temperature,
}

/// Metadata for a specific unit.
#[derive(Debug, Clone)]
struct UnitDefinition {
    /// Unit symbol or abbreviation (e.g., "kg").
    pub symbol: String,
    /// Compatible category.
    pub category: UnitCategory,
    /// Factor to convert this unit to the category's base unit.
    pub factor: f64,
}

/// The core conversion engine.
pub struct Converter {
    /// User configuration for sorting and limits.
    config: Config,
    /// Cached currency rates.
    cache: Cache,
    /// Registry of all supported physical units.
    units: HashMap<String, UnitDefinition>,
}

impl Converter {
    /// Creates a new `Converter` with the provided configuration and cache.
    #[must_use]
    pub fn new(config: Config, cache: Cache) -> Self {
        let mut units = HashMap::new();

        // Length Units (Base: meter)
        add_unit(&mut units, "m", UnitCategory::Length, 1.0);
        add_unit(&mut units, "meter", UnitCategory::Length, 1.0);
        add_unit(&mut units, "meters", UnitCategory::Length, 1.0);
        add_unit(&mut units, "cm", UnitCategory::Length, 0.01);
        add_unit(&mut units, "mm", UnitCategory::Length, 0.001);
        add_unit(&mut units, "km", UnitCategory::Length, 1000.0);
        add_unit(&mut units, "in", UnitCategory::Length, 0.0254);
        add_unit(&mut units, "inch", UnitCategory::Length, 0.0254);
        add_unit(&mut units, "inches", UnitCategory::Length, 0.0254);
        add_unit(&mut units, "ft", UnitCategory::Length, 0.3048);
        add_unit(&mut units, "foot", UnitCategory::Length, 0.3048);
        add_unit(&mut units, "feet", UnitCategory::Length, 0.3048);
        add_unit(&mut units, "yd", UnitCategory::Length, 0.9144);
        add_unit(&mut units, "yard", UnitCategory::Length, 0.9144);
        add_unit(&mut units, "yards", UnitCategory::Length, 0.9144);
        add_unit(&mut units, "mi", UnitCategory::Length, 1609.344);
        add_unit(&mut units, "mile", UnitCategory::Length, 1609.344);
        add_unit(&mut units, "miles", UnitCategory::Length, 1609.344);

        // Weight Units (Base: gram)
        add_unit(&mut units, "g", UnitCategory::Weight, 1.0);
        add_unit(&mut units, "gram", UnitCategory::Weight, 1.0);
        add_unit(&mut units, "grams", UnitCategory::Weight, 1.0);
        add_unit(&mut units, "kg", UnitCategory::Weight, 1000.0);
        add_unit(&mut units, "kilogram", UnitCategory::Weight, 1000.0);
        add_unit(&mut units, "kilograms", UnitCategory::Weight, 1000.0);
        add_unit(&mut units, "mg", UnitCategory::Weight, 0.001);
        add_unit(&mut units, "lb", UnitCategory::Weight, 453.592_37);
        add_unit(&mut units, "pound", UnitCategory::Weight, 453.592_37);
        add_unit(&mut units, "pounds", UnitCategory::Weight, 453.592_37);
        add_unit(&mut units, "oz", UnitCategory::Weight, 28.349_523_125);
        add_unit(&mut units, "ounce", UnitCategory::Weight, 28.349_523_125);
        add_unit(&mut units, "ounces", UnitCategory::Weight, 28.349_523_125);

        // Temperature Units (Base: Celsius)
        // Special case handling will be used for calculation.
        add_unit(&mut units, "C", UnitCategory::Temperature, 1.0);
        add_unit(&mut units, "F", UnitCategory::Temperature, 1.0);
        add_unit(&mut units, "K", UnitCategory::Temperature, 1.0);

        Self {
            config,
            cache,
            units,
        }
    }

    /// Returns a deduplicated list of all supported unit and currency symbols.
    #[must_use]
    pub fn get_all_units(&self) -> Vec<String> {
        let mut units: Vec<String> = self.units.keys().cloned().collect();
        units.extend(self.cache.rates.keys().cloned());
        units.sort();
        units.dedup();
        units
    }

    /// Converts a numeric value from one unit to all compatible target units.
    ///
    /// # Errors
    /// Returns an error if the input unit is unknown or if the conversion fails.
    pub fn convert(&self, value: f64, from_unit: &str) -> Result<ConversionResult> {
        let (category, base_value) = self.resolve_base(value, from_unit)?;
        let mut outputs = Vec::new();

        match category {
            UnitCategory::Currency => {
                for (symbol, rate) in &self.cache.rates {
                    if symbol != from_unit {
                        outputs.push(ConvertedValue {
                            value: base_value * rate,
                            unit: symbol.clone(),
                        });
                    }
                }
            }
            UnitCategory::Temperature => {
                let targets = ["C", "F", "K"];
                for target in targets {
                    if target != from_unit {
                        outputs.push(ConvertedValue {
                            value: convert_temperature(base_value, from_unit, target),
                            unit: target.to_string(),
                        });
                    }
                }
            }
            _ => {
                for unit_def in self.units.values() {
                    if unit_def.category == category && unit_def.symbol != from_unit {
                        outputs.push(ConvertedValue {
                            value: base_value / unit_def.factor,
                            unit: unit_def.symbol.clone(),
                        });
                    }
                }
            }
        }

        // Deduplicate units (e.g., "m" and "meter" point to same symbol)
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
            input_unit: from_unit.to_string(),
            outputs,
        })
    }

    /// Resolves the unit category and the value in its base unit.
    fn resolve_base(&self, value: f64, unit: &str) -> Result<(UnitCategory, f64)> {
        if let Some(unit_def) = self.units.get(unit) {
            if unit_def.category == UnitCategory::Temperature {
                return Ok((UnitCategory::Temperature, value));
            }
            return Ok((unit_def.category, value * unit_def.factor));
        }

        if self.cache.rates.contains_key(unit) {
            let eur_rate = self.cache.rates.get(unit).unwrap_or(&1.0);
            return Ok((UnitCategory::Currency, value / eur_rate));
        }

        Err(anyhow!("Unsupported unit: {unit}"))
    }
}

/// Helper to add a unit definition to a hashmap.
fn add_unit(
    map: &mut HashMap<String, UnitDefinition>,
    symbol: &str,
    category: UnitCategory,
    factor: f64,
) {
    map.insert(
        symbol.to_string(),
        UnitDefinition {
            symbol: symbol.to_string(),
            category,
            factor,
        },
    );
}

/// Specialized temperature conversion logic.
fn convert_temperature(value: f64, from: &str, to: &str) -> f64 {
    let in_c = match from {
        "F" => (value - 32.0) * 5.0 / 9.0,
        "K" => value - 273.15,
        _ => value,
    };

    match to {
        "F" => (in_c * 9.0 / 5.0) + 32.0,
        "K" => in_c + 273.15,
        _ => in_c,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;

    #[test]
    fn test_length_conversion() {
        let config = Config::default();
        let cache = Cache::default();
        let converter = Converter::new(config, cache);

        let res = converter.convert(1.0, "m").unwrap();
        let cm = res.outputs.iter().find(|o| o.unit == "cm").unwrap();
        assert!((cm.value - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_currency_conversion() {
        let config = Config::default();
        let mut cache = Cache::default();
        cache.rates.insert("USD".to_string(), 1.1);
        cache.rates.insert("EUR".to_string(), 1.0);
        let converter = Converter::new(config, cache);

        let res = converter.convert(10.0, "EUR").unwrap();
        let usd = res.outputs.iter().find(|o| o.unit == "USD").unwrap();
        assert!((usd.value - 11.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_temperature_conversion() {
        let config = Config::default();
        let cache = Cache::default();
        let converter = Converter::new(config, cache);

        let res = converter.convert(0.0, "C").unwrap();
        let f = res.outputs.iter().find(|o| o.unit == "F").unwrap();
        assert!((f.value - 32.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_deduplication_and_sorting() {
        let config = Config {
            favorites: vec!["ft".to_string()],
            ..Config::default()
        };
        let cache = Cache::default();
        let converter = Converter::new(config, cache);

        let res = converter.convert(1.0, "m").unwrap();

        // Ensure "meter", "meters", "m" are not all present as outputs
        let m_count = res.outputs.iter().filter(|o| o.unit == "m").count();
        assert_eq!(m_count, 0, "Input unit should not be in output");

        // "ft" should be first because it's a favorite
        assert_eq!(res.outputs[0].unit, "ft");
    }

    #[test]
    fn test_list_limit() {
        let config = Config {
            list_size: 2,
            ..Config::default()
        };
        let cache = Cache::default();
        let converter = Converter::new(config, cache);

        let res = converter.convert(1.0, "m").unwrap();
        assert_eq!(res.outputs.len(), 2);
    }
}
