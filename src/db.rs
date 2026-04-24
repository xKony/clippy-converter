use anyhow::{Context, Result};
use directories::ProjectDirs;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::path::PathBuf;
use std::sync::Arc;

use crate::models::{RateSource, UnitCategory, UnitEntry};

/// Schema for unified units and currency rates.
const UNITS_TABLE: TableDefinition<&str, UnitEntry> = TableDefinition::new("units_v2");

/// Schema for unit aliases (e.g., "meters" -> "m").
const ALIASES_TABLE: TableDefinition<&str, &str> = TableDefinition::new("aliases");

// Implement redb::Value for UnitEntry using bincode serialization.
impl redb::Value for UnitEntry {
    type SelfType<'a> = Self;
    type AsBytes<'a> = Vec<u8>;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        bincode::deserialize(data).unwrap_or(Self {
            factor: 1.0,
            offset: 0.0,
            category: 0,
            timestamp: 0,
            source: 0,
        })
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a> {
        bincode::serialize(value).unwrap_or_default()
    }

    fn type_name() -> redb::TypeName {
        redb::TypeName::new("UnitEntry")
    }
}

/// Thread-safe wrapper around the redb database.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Database>,
}

impl Db {
    /// Opens the database at the default user cache location.
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be determined or if the database fails to open.
    pub fn open() -> Result<Self> {
        let path = get_db_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create database directory at {}",
                    parent.display()
                )
            })?;
        }

        let db = Database::builder()
            .create(path)
            .context("Failed to initialize redb database")?;

        // Ensure tables are created
        let write_txn = db.begin_write().context("Failed to begin init transaction")?;
        {
            let _ = write_txn
                .open_table(UNITS_TABLE)
                .context("Failed to create units table")?;
            let _ = write_txn
                .open_table(ALIASES_TABLE)
                .context("Failed to create aliases table")?;
        }
        write_txn.commit().context("Failed to commit init transaction")?;

        Ok(Self {
            inner: Arc::new(db),
        })
    }

    /// Creates a test instance of the database.
    #[cfg(test)]
    #[must_use]
    pub const fn open_for_test(inner: Arc<Database>) -> Self {
        Self { inner }
    }

    /// Updates a rate in the database. If the entry exists, it is only updated if the new
    /// data is from a higher-priority source (Crypto) or is more recent.
    ///
    /// # Errors
    /// Returns an error if the transaction fails.
    pub fn update_rate(
        &self,
        symbol: &str,
        price: f64,
        timestamp: i64,
        source: RateSource,
    ) -> Result<()> {
        let write_txn = self
            .inner
            .begin_write()
            .context("Failed to begin write transaction")?;
        {
            let mut units_table = write_txn
                .open_table(UNITS_TABLE)
                .context("Failed to open units table")?;

            let should_update = units_table
                .get(symbol)
                .context("Failed to read existing unit")?
                .is_none_or(|existing| {
                    let existing_val: UnitEntry = existing.value();
                    (source as u8 > existing_val.source)
                        || (source as u8 == existing_val.source
                            && timestamp > existing_val.timestamp)
                });

            if should_update {
                // Factor must be "EUR per 1 Unit" so that Base_EUR = Value * Factor.
                // - Fiat rates are "Units per 1 EUR" (e.g. 1.08 USD/EUR), so Factor = 1/price.
                // - Crypto rates are already "EUR per 1 Unit" (e.g. 60000 EUR/BTC), so Factor = price.
                let factor = if source == RateSource::Fiat {
                    if price == 0.0 { 0.0 } else { 1.0 / price }
                } else {
                    price
                };

                units_table
                    .insert(
                        symbol,
                        UnitEntry {
                            factor,
                            offset: 0.0,
                            category: UnitCategory::Currency as u8,
                            timestamp,
                            source: source as u8,
                        },
                    )
                    .context("Failed to insert into units table")?;
            }
        }
        write_txn
            .commit()
            .context("Failed to commit write transaction")?;
        Ok(())
    }

    /// Returns a list of all symbols stored in the database.
    ///
    /// # Errors
    /// Returns an error if the read transaction fails or if iteration fails.
    pub fn get_all_symbols(&self) -> Result<Vec<String>> {
        let read_txn = self
            .inner
            .begin_read()
            .context("Failed to begin read transaction")?;
        let table = read_txn
            .open_table(UNITS_TABLE)
            .context("Failed to open units table")?;
        let mut symbols = Vec::new();
        for result in table.iter().context("Failed to iterate table")? {
            let (key, _) = result.context("Failed to read row")?;
            symbols.push(key.value().to_string());
        }
        Ok(symbols)
    }

    /// Returns all canonical symbols with their associated aliases.
    ///
    /// # Errors
    /// Returns an error if the read transaction or iteration fails.
    pub fn get_all_units_with_aliases(&self) -> Result<std::collections::HashMap<String, Vec<String>>> {
        let read_txn = self
            .inner
            .begin_read()
            .context("Failed to begin read transaction")?;
        let units_table = read_txn
            .open_table(UNITS_TABLE)
            .context("Failed to open units table")?;
        let alias_table = read_txn
            .open_table(ALIASES_TABLE)
            .context("Failed to open aliases table")?;

        let mut unit_map = std::collections::HashMap::new();

        // 1. Collect all canonical symbols
        for result in units_table.iter().context("Failed to iterate units")? {
            let (key, _) = result.context("Failed to read unit row")?;
            unit_map.insert(key.value().to_string(), Vec::new());
        }

        // 2. Collect all aliases and group them by canonical symbol
        for result in alias_table.iter().context("Failed to iterate aliases")? {
            let (alias, canonical) = result.context("Failed to read alias row")?;
            let canonical_str = canonical.value();
            let alias_str = alias.value();

            if let Some(aliases) = unit_map.get_mut(canonical_str) {
                // Only add if it's actually an alias (not just the lowercase version of the symbol itself)
                if alias_str.to_lowercase() != canonical_str.to_lowercase()
                    && !aliases.contains(&alias_str.to_string())
                {
                    aliases.push(alias_str.to_string());
                }
            }
        }

        Ok(unit_map)
    }

    /// Resolves a unit symbol or alias to its canonical form.
    ///
    /// # Errors
    /// Returns an error if the database read fails.
    pub fn resolve_symbol(&self, symbol: &str) -> Result<String> {
        let read_txn = self
            .inner
            .begin_read()
            .context("Failed to begin read transaction")?;
        let alias_table = read_txn
            .open_table(ALIASES_TABLE)
            .context("Failed to open aliases table")?;

        // 1. Check direct alias (e.g., "kilometers" -> "km")
        if let Some(canonical) = alias_table.get(symbol).context("Failed to read alias")? {
            return Ok(canonical.value().to_string());
        }

        // 2. Check lowercase alias (e.g., "Celsius" -> "celsius" -> "C")
        let lower = symbol.to_lowercase();
        if let Some(canonical) = alias_table
            .get(lower.as_str())
            .context("Failed to read lowercase alias")?
        {
            return Ok(canonical.value().to_string());
        }

        Ok(symbol.to_string())
    }

    /// Retrieves a unit entry for a given symbol.
    ///
    /// # Errors
    /// Returns an error if the read transaction fails.
    pub fn get_unit(&self, symbol: &str) -> Result<Option<UnitEntry>> {
        let read_txn = self
            .inner
            .begin_read()
            .context("Failed to begin read transaction")?;
        let table = read_txn
            .open_table(UNITS_TABLE)
            .context("Failed to open units table")?;
        let result = table.get(symbol).context("Failed to query symbol")?;
        Ok(result.map(|r| r.value()))
    }

    /// Retrieves all units belonging to a specific category.
    ///
    /// # Errors
    /// Returns an error if the read transaction or iteration fails.
    pub fn get_category_units(&self, category: u8) -> Result<Vec<(String, UnitEntry)>> {
        let read_txn = self
            .inner
            .begin_read()
            .context("Failed to begin read transaction")?;
        let table = read_txn
            .open_table(UNITS_TABLE)
            .context("Failed to open units table")?;
        let mut units = Vec::new();
        for result in table.iter().context("Failed to iterate units")? {
            let (key, value) = result.context("Failed to read unit row")?;
            let entry = value.value();
            if entry.category == category {
                units.push((key.value().to_string(), entry));
            }
        }
        Ok(units)
    }

    /// Updates a unit in the database.
    ///
    /// # Errors
    /// Returns an error if the transaction fails.
    pub fn update_unit(
        &self,
        symbol: &str,
        factor: f64,
        offset: f64,
        category: UnitCategory,
        source: RateSource,
    ) -> Result<()> {
        let write_txn = self
            .inner
            .begin_write()
            .context("Failed to begin write transaction")?;
        {
            let mut table = write_txn
                .open_table(UNITS_TABLE)
                .context("Failed to open units table")?;
            table
                .insert(
                    symbol,
                    UnitEntry {
                        factor,
                        offset,
                        category: category as u8,
                        timestamp: chrono::Utc::now().timestamp(),
                        source: source as u8,
                    },
                )
                .context("Failed to insert unit")?;
        }
        write_txn.commit().context("Failed to commit unit update")?;
        Ok(())
    }

    /// Initializes the database with static units and their aliases.
    ///
    /// # Errors
    /// Returns an error if any transaction fails.
    pub fn init_static_units(&self) -> Result<()> {
        let write_txn = self
            .inner
            .begin_write()
            .context("Failed to begin write transaction")?;
        {
            let mut units = write_txn
                .open_table(UNITS_TABLE)
                .context("Failed to open units table")?;
            let mut aliases = write_txn
                .open_table(ALIASES_TABLE)
                .context("Failed to open aliases table")?;

            init_length_units(&mut units, &mut aliases)?;
            init_weight_units(&mut units, &mut aliases)?;
            init_temperature_units(&mut units, &mut aliases)?;
            init_time_units(&mut units, &mut aliases)?;
        }
        write_txn.commit().context("Failed to commit static units")?;
        Ok(())
    }
}

/// Helper to add a unit and its variations to the database.
fn add_unit_static(
    units: &mut redb::Table<&str, UnitEntry>,
    aliases: &mut redb::Table<&str, &str>,
    sym: &str,
    cat: UnitCategory,
    factor: f64,
    offset: f64,
    variations: &[&str],
) -> Result<()> {
    units
        .insert(
            sym,
            UnitEntry {
                factor,
                offset,
                category: cat as u8,
                timestamp: 0,
                source: RateSource::Static as u8,
            },
        )
        .context("Failed to insert static unit")?;
    for v in variations {
        aliases
            .insert(*v, sym)
            .context("Failed to insert alias")?;
        aliases
            .insert(v.to_lowercase().as_str(), sym)
            .context("Failed to insert lowercase alias")?;
    }
    Ok(())
}

fn init_length_units(
    units: &mut redb::Table<&str, UnitEntry>,
    aliases: &mut redb::Table<&str, &str>,
) -> Result<()> {
    add_unit_static(
        units,
        aliases,
        "m",
        UnitCategory::Length,
        1.0,
        0.0,
        &["meter", "meters", "metre", "metres"],
    )?;
    add_unit_static(
        units,
        aliases,
        "km",
        UnitCategory::Length,
        1000.0,
        0.0,
        &["kilometer", "kilometers", "kilometre", "kilometres"],
    )?;
    add_unit_static(
        units,
        aliases,
        "cm",
        UnitCategory::Length,
        0.01,
        0.0,
        &["centimeter", "centimeters", "centimetre", "centimetres"],
    )?;
    add_unit_static(
        units,
        aliases,
        "mm",
        UnitCategory::Length,
        0.001,
        0.0,
        &["millimeter", "millimeters", "millimetre", "millimetres"],
    )?;
    add_unit_static(
        units,
        aliases,
        "in",
        UnitCategory::Length,
        0.0254,
        0.0,
        &["inch", "inches"],
    )?;
    add_unit_static(
        units,
        aliases,
        "ft",
        UnitCategory::Length,
        0.3048,
        0.0,
        &["foot", "feet", "ft."],
    )?;
    add_unit_static(
        units,
        aliases,
        "yd",
        UnitCategory::Length,
        0.9144,
        0.0,
        &["yard", "yards"],
    )?;
    add_unit_static(
        units,
        aliases,
        "mi",
        UnitCategory::Length,
        1609.344,
        0.0,
        &["mile", "miles"],
    )?;
    Ok(())
}

fn init_weight_units(
    units: &mut redb::Table<&str, UnitEntry>,
    aliases: &mut redb::Table<&str, &str>,
) -> Result<()> {
    add_unit_static(units, aliases, "g", UnitCategory::Weight, 1.0, 0.0, &[
        "gram", "grams", "gr",
    ])?;
    add_unit_static(
        units,
        aliases,
        "kg",
        UnitCategory::Weight,
        1000.0,
        0.0,
        &["kilogram", "kilograms", "kilo"],
    )?;
    add_unit_static(
        units,
        aliases,
        "mg",
        UnitCategory::Weight,
        0.001,
        0.0,
        &["milligram", "milligrams"],
    )?;
    add_unit_static(
        units,
        aliases,
        "lb",
        UnitCategory::Weight,
        453.592_37,
        0.0,
        &["pound", "pounds", "lbs"],
    )?;
    add_unit_static(
        units,
        aliases,
        "oz",
        UnitCategory::Weight,
        28.349_523_125,
        0.0,
        &["ounce", "ounces"],
    )?;
    Ok(())
}

fn init_temperature_units(
    units: &mut redb::Table<&str, UnitEntry>,
    aliases: &mut redb::Table<&str, &str>,
) -> Result<()> {
    add_unit_static(
        units,
        aliases,
        "C",
        UnitCategory::Temperature,
        1.0,
        0.0,
        &["Celsius", "celsius", "centigrade"],
    )?;
    add_unit_static(
        units,
        aliases,
        "F",
        UnitCategory::Temperature,
        5.0 / 9.0,
        -32.0,
        &["Fahrenheit", "fahrenheit"],
    )?;
    add_unit_static(
        units,
        aliases,
        "K",
        UnitCategory::Temperature,
        1.0,
        -273.15,
        &["Kelvin", "kelvin"],
    )?;
    Ok(())
}

fn init_time_units(
    units: &mut redb::Table<&str, UnitEntry>,
    aliases: &mut redb::Table<&str, &str>,
) -> Result<()> {
    add_unit_static(units, aliases, "s", UnitCategory::Time, 1.0, 0.0, &[
        "second", "seconds", "sec",
    ])?;
    add_unit_static(
        units,
        aliases,
        "ms",
        UnitCategory::Time,
        0.001,
        0.0,
        &["millisecond", "milliseconds"],
    )?;
    add_unit_static(
        units,
        aliases,
        "min",
        UnitCategory::Time,
        60.0,
        0.0,
        &["minute", "minutes"],
    )?;
    add_unit_static(
        units,
        aliases,
        "h",
        UnitCategory::Time,
        3600.0,
        0.0,
        &["hour", "hours"],
    )?;
    Ok(())
}

fn get_db_path() -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "clippy", "clippy-converter")
        .context("Could not determine application cache directory")?;
    Ok(proj_dirs.cache_dir().join("rates.redb"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_db_update_priority() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        // 1. Insert Fiat rate
        // 1 EUR = 50000 BTC (Incredibly cheap BTC in this test!)
        // factor = 1/50000
        db.update_rate("BTC", 50000.0, 1000, RateSource::Fiat)
            .unwrap();
        let entry = db.get_unit("BTC").unwrap().unwrap();
        assert_eq!(entry.source, RateSource::Fiat as u8);
        assert!((entry.factor - (1.0 / 50000.0)).abs() < f64::EPSILON);

        // 2. Insert Crypto rate (Higher priority)
        // 1 BTC = 51000 EUR
        // factor = 51000
        db.update_rate("BTC", 51000.0, 900, RateSource::Crypto)
            .unwrap();
        let entry = db.get_unit("BTC").unwrap().unwrap();
        assert_eq!(entry.source, RateSource::Crypto as u8);
        assert_eq!(entry.factor, 51000.0);

        // 3. Try to overwrite Crypto with newer Fiat (Should fail due to lower source priority)
        db.update_rate("BTC", 49000.0, 1100, RateSource::Fiat)
            .unwrap();
        let entry = db.get_unit("BTC").unwrap().unwrap();
        assert_eq!(entry.source, RateSource::Crypto as u8);
        assert_eq!(entry.factor, 51000.0);
    }

    #[test]
    fn test_resolve_symbol() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        db.init_static_units().unwrap();

        // Direct match
        assert_eq!(db.resolve_symbol("m").unwrap(), "m");

        // Alias match
        assert_eq!(db.resolve_symbol("meters").unwrap(), "m");

        // Case-insensitive alias match
        assert_eq!(db.resolve_symbol("Celsius").unwrap(), "C");

        // Unknown unit falls back to itself
        assert_eq!(db.resolve_symbol("unknown").unwrap(), "unknown");
    }

    #[test]
    fn test_init_static_units() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        db.init_static_units().unwrap();

        let m = db.get_unit("m").unwrap().unwrap();
        assert_eq!(m.category, UnitCategory::Length as u8);
        assert_eq!(m.factor, 1.0);

        let km = db.get_unit("km").unwrap().unwrap();
        assert_eq!(km.factor, 1000.0);

        let f = db.get_unit("F").unwrap().unwrap();
        assert_eq!(f.category, UnitCategory::Temperature as u8);
        assert!((f.factor - 5.0 / 9.0).abs() < f64::EPSILON);
        assert_eq!(f.offset, -32.0);
    }
}
