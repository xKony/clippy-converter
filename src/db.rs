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
        // redb's `Value` trait cannot surface a `Result`, so a failed deserialization is
        // mapped to an unmistakable sentinel instead of a plausible-looking default (which
        // previously masked corruption as a silent `factor: 1.0` conversion). Callers detect
        // this via `UnitEntry::is_corrupt`.
        bincode::deserialize(data).unwrap_or_else(|_| Self::corrupt_sentinel())
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a> {
        bincode::serialize(value).unwrap_or_default()
    }

    fn type_name() -> redb::TypeName {
        redb::TypeName::new("UnitEntry")
    }
}

impl UnitEntry {
    /// Sentinel timestamp used to mark an entry that failed to deserialize.
    const CORRUPT_TIMESTAMP: i64 = i64::MIN;
    /// Sentinel source used to mark an entry that failed to deserialize.
    const CORRUPT_SOURCE: u8 = u8::MAX;

    /// Builds a recognizable sentinel entry for bytes that failed to deserialize.
    const fn corrupt_sentinel() -> Self {
        Self {
            factor: f64::NAN,
            offset: 0.0,
            category: 0,
            timestamp: Self::CORRUPT_TIMESTAMP,
            source: Self::CORRUPT_SOURCE,
        }
    }

    /// Returns `true` if this entry is a corruption sentinel produced when the stored bytes
    /// failed to deserialize, rather than real conversion data.
    #[must_use]
    pub const fn is_corrupt(&self) -> bool {
        self.timestamp == Self::CORRUPT_TIMESTAMP && self.source == Self::CORRUPT_SOURCE
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
        let write_txn = db
            .begin_write()
            .context("Failed to begin init transaction")?;
        {
            let _ = write_txn
                .open_table(UNITS_TABLE)
                .context("Failed to create units table")?;
            let _ = write_txn
                .open_table(ALIASES_TABLE)
                .context("Failed to create aliases table")?;
        }
        write_txn
            .commit()
            .context("Failed to commit init transaction")?;

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
    /// This is a thin wrapper around [`Db::update_rates_batch`] for a single symbol; callers
    /// updating many symbols at once (e.g. after a bulk API refresh) should prefer the batch
    /// method to avoid opening one write transaction per symbol.
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
        self.update_rates_batch([(symbol.to_string(), price)], timestamp, source)?;
        Ok(())
    }

    /// Updates many rates in a single write transaction, applying the same priority rules as
    /// [`Db::update_rate`] (a higher-priority source, e.g. Crypto over Fiat, always wins; ties
    /// on source are broken by the newer timestamp). All entries share the same `timestamp` and
    /// `source`, matching a single batch API refresh.
    ///
    /// Intended for workers refreshing many symbols at once (e.g. all fiat or all crypto rates)
    /// so that only one write transaction and one table handle are used for the whole batch.
    ///
    /// # Errors
    /// Returns an error if the transaction fails.
    ///
    /// Returns the number of symbols that were actually updated (i.e. that passed the priority
    /// check), which may be less than the number of input rates.
    pub fn update_rates_batch(
        &self,
        rates: impl IntoIterator<Item = (String, f64)>,
        timestamp: i64,
        source: RateSource,
    ) -> Result<usize> {
        let write_txn = self
            .inner
            .begin_write()
            .context("Failed to begin write transaction")?;
        let mut updated_count = 0_usize;
        {
            let mut units_table = write_txn
                .open_table(UNITS_TABLE)
                .context("Failed to open units table")?;

            for (symbol, price) in rates {
                let should_update = units_table
                    .get(symbol.as_str())
                    .context("Failed to read existing unit")?
                    .is_none_or(|existing| {
                        let existing_val: UnitEntry = existing.value();
                        existing_val.is_corrupt()
                            || (source as u8 > existing_val.source)
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
                            symbol.as_str(),
                            UnitEntry {
                                factor,
                                offset: 0.0,
                                category: UnitCategory::Currency as u8,
                                timestamp,
                                source: source as u8,
                            },
                        )
                        .context("Failed to insert into units table")?;
                    updated_count += 1;
                }
            }
        }
        write_txn
            .commit()
            .context("Failed to commit write transaction")?;
        Ok(updated_count)
    }

    /// Returns all canonical symbols with their associated aliases.
    ///
    /// # Errors
    /// Returns an error if the read transaction or iteration fails.
    pub fn get_all_units_with_aliases(
        &self,
    ) -> Result<std::collections::HashMap<String, Vec<String>>> {
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

        // 1. Collect all canonical symbols, skipping entries that failed to deserialize.
        for result in units_table.iter().context("Failed to iterate units")? {
            let (key, value) = result.context("Failed to read unit row")?;
            if value.value().is_corrupt() {
                continue;
            }
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
    /// Returns an error if the read transaction fails, or if the stored entry is corrupt
    /// (i.e. failed to deserialize).
    pub fn get_unit(&self, symbol: &str) -> Result<Option<UnitEntry>> {
        let read_txn = self
            .inner
            .begin_read()
            .context("Failed to begin read transaction")?;
        let table = read_txn
            .open_table(UNITS_TABLE)
            .context("Failed to open units table")?;
        let result = table.get(symbol).context("Failed to query symbol")?;
        let Some(entry) = result.map(|r| r.value()) else {
            return Ok(None);
        };
        if entry.is_corrupt() {
            return Err(anyhow::anyhow!("corrupt unit entry for {symbol}"));
        }
        Ok(Some(entry))
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
            if entry.is_corrupt() {
                continue;
            }
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

            seed_static_units(&mut units, &mut aliases)?;
        }
        write_txn
            .commit()
            .context("Failed to commit static units")?;
        Ok(())
    }
}

struct StaticUnit {
    symbol: &'static str,
    category: UnitCategory,
    factor: f64,
    offset: f64,
    aliases: &'static [&'static str],
}

/// Static length/weight/temperature/time units seeded on first launch.
/// Temperature: Base = (Input + Offset) * Factor, Target = (Base / Factor) - Offset.
/// Celsius is the base (factor=1, offset=0); F uses (F-32)*5/9; K uses K-273.15.
const STATIC_UNITS: &[StaticUnit] = &[
    StaticUnit {
        symbol: "m",
        category: UnitCategory::Length,
        factor: 1.0,
        offset: 0.0,
        aliases: &["meter", "meters", "metre", "metres"],
    },
    StaticUnit {
        symbol: "km",
        category: UnitCategory::Length,
        factor: 1000.0,
        offset: 0.0,
        aliases: &["kilometer", "kilometers", "kilometre", "kilometres"],
    },
    StaticUnit {
        symbol: "cm",
        category: UnitCategory::Length,
        factor: 0.01,
        offset: 0.0,
        aliases: &["centimeter", "centimeters", "centimetre", "centimetres"],
    },
    StaticUnit {
        symbol: "mm",
        category: UnitCategory::Length,
        factor: 0.001,
        offset: 0.0,
        aliases: &["millimeter", "millimeters", "millimetre", "millimetres"],
    },
    StaticUnit {
        symbol: "in",
        category: UnitCategory::Length,
        factor: 0.0254,
        offset: 0.0,
        aliases: &["inch", "inches"],
    },
    StaticUnit {
        symbol: "ft",
        category: UnitCategory::Length,
        factor: 0.3048,
        offset: 0.0,
        aliases: &["foot", "feet", "ft."],
    },
    StaticUnit {
        symbol: "yd",
        category: UnitCategory::Length,
        factor: 0.9144,
        offset: 0.0,
        aliases: &["yard", "yards"],
    },
    StaticUnit {
        symbol: "mi",
        category: UnitCategory::Length,
        factor: 1609.344,
        offset: 0.0,
        aliases: &["mile", "miles"],
    },
    StaticUnit {
        symbol: "g",
        category: UnitCategory::Weight,
        factor: 1.0,
        offset: 0.0,
        aliases: &["gram", "grams", "gr"],
    },
    StaticUnit {
        symbol: "kg",
        category: UnitCategory::Weight,
        factor: 1000.0,
        offset: 0.0,
        aliases: &["kilogram", "kilograms", "kilo"],
    },
    StaticUnit {
        symbol: "mg",
        category: UnitCategory::Weight,
        factor: 0.001,
        offset: 0.0,
        aliases: &["milligram", "milligrams"],
    },
    StaticUnit {
        symbol: "lb",
        category: UnitCategory::Weight,
        factor: 453.592_37,
        offset: 0.0,
        aliases: &["pound", "pounds", "lbs"],
    },
    StaticUnit {
        symbol: "oz",
        category: UnitCategory::Weight,
        factor: 28.349_523_125,
        offset: 0.0,
        aliases: &["ounce", "ounces"],
    },
    StaticUnit {
        symbol: "C",
        category: UnitCategory::Temperature,
        factor: 1.0,
        offset: 0.0,
        aliases: &["Celsius", "celsius", "centigrade"],
    },
    StaticUnit {
        symbol: "F",
        category: UnitCategory::Temperature,
        factor: 5.0 / 9.0,
        offset: -32.0,
        aliases: &["Fahrenheit", "fahrenheit"],
    },
    StaticUnit {
        symbol: "K",
        category: UnitCategory::Temperature,
        factor: 1.0,
        offset: -273.15,
        aliases: &["Kelvin", "kelvin"],
    },
    StaticUnit {
        symbol: "s",
        category: UnitCategory::Time,
        factor: 1.0,
        offset: 0.0,
        aliases: &["second", "seconds", "sec"],
    },
    StaticUnit {
        symbol: "ms",
        category: UnitCategory::Time,
        factor: 0.001,
        offset: 0.0,
        aliases: &["millisecond", "milliseconds"],
    },
    StaticUnit {
        symbol: "min",
        category: UnitCategory::Time,
        factor: 60.0,
        offset: 0.0,
        aliases: &["minute", "minutes"],
    },
    StaticUnit {
        symbol: "h",
        category: UnitCategory::Time,
        factor: 3600.0,
        offset: 0.0,
        aliases: &["hour", "hours"],
    },
];

fn seed_static_units(
    units: &mut redb::Table<&str, UnitEntry>,
    aliases: &mut redb::Table<&str, &str>,
) -> Result<()> {
    for unit in STATIC_UNITS {
        add_unit_static(units, aliases, unit)?;
    }
    Ok(())
}

/// Helper to add a unit and its variations to the database.
/// Skips insertion if the unit already exists to avoid overwriting on every launch.
fn add_unit_static(
    units: &mut redb::Table<&str, UnitEntry>,
    aliases: &mut redb::Table<&str, &str>,
    unit: &StaticUnit,
) -> Result<()> {
    // Only insert if the unit doesn't already exist
    let exists = units
        .get(unit.symbol)
        .context("Failed to check existing unit")?
        .is_some();

    if !exists {
        units
            .insert(
                unit.symbol,
                UnitEntry {
                    factor: unit.factor,
                    offset: unit.offset,
                    category: unit.category as u8,
                    timestamp: 0,
                    source: RateSource::Static as u8,
                },
            )
            .context("Failed to insert static unit")?;
    }

    for v in unit.aliases {
        aliases
            .insert(*v, unit.symbol)
            .context("Failed to insert alias")?;
        aliases
            .insert(v.to_lowercase().as_str(), unit.symbol)
            .context("Failed to insert lowercase alias")?;
    }
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
    fn test_update_rates_batch_applies_all_in_one_transaction() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        let rates = [
            ("USD".to_string(), 1.08),
            ("PLN".to_string(), 4.0),
            ("GBP".to_string(), 0.85),
        ];
        let updated = db
            .update_rates_batch(rates, 1000, RateSource::Fiat)
            .unwrap();
        assert_eq!(updated, 3);

        let usd = db.get_unit("USD").unwrap().unwrap();
        assert!((usd.factor - (1.0 / 1.08)).abs() < f64::EPSILON);
        assert_eq!(usd.source, RateSource::Fiat as u8);

        let pln = db.get_unit("PLN").unwrap().unwrap();
        assert!((pln.factor - 0.25).abs() < f64::EPSILON);

        let gbp = db.get_unit("GBP").unwrap().unwrap();
        assert!((gbp.factor - (1.0 / 0.85)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_update_rates_batch_respects_source_priority() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        // Seed BTC from the higher-priority Crypto source.
        db.update_rate("BTC", 50000.0, 1000, RateSource::Crypto)
            .unwrap();

        // A later Fiat batch should skip BTC (lower priority) but still apply ETH (new symbol).
        let rates = [("BTC".to_string(), 49000.0), ("ETH".to_string(), 3000.0)];
        let updated = db
            .update_rates_batch(rates, 2000, RateSource::Fiat)
            .unwrap();
        assert_eq!(updated, 1);

        let btc = db.get_unit("BTC").unwrap().unwrap();
        assert_eq!(btc.source, RateSource::Crypto as u8);
        assert_eq!(btc.factor, 50000.0);

        let eth = db.get_unit("ETH").unwrap().unwrap();
        assert_eq!(eth.source, RateSource::Fiat as u8);
    }

    #[test]
    fn test_update_rates_batch_newer_timestamp_wins_within_same_source() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        db.update_rate("PLN", 4.0, 1000, RateSource::Fiat).unwrap();

        // Newer timestamp, same source: should update.
        let updated = db
            .update_rates_batch([("PLN".to_string(), 4.5)], 2000, RateSource::Fiat)
            .unwrap();
        assert_eq!(updated, 1);
        let pln = db.get_unit("PLN").unwrap().unwrap();
        assert!((pln.factor - (1.0 / 4.5)).abs() < f64::EPSILON);

        // Older timestamp, same source: should be skipped.
        let updated = db
            .update_rates_batch([("PLN".to_string(), 5.0)], 500, RateSource::Fiat)
            .unwrap();
        assert_eq!(updated, 0);
        let pln = db.get_unit("PLN").unwrap().unwrap();
        assert!((pln.factor - (1.0 / 4.5)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_corrupt_bytes_produce_sentinel_entry() {
        // Fewer bytes than `UnitEntry`'s bincode encoding requires, so deserialization fails
        // and `from_bytes` must fall back to the corruption sentinel instead of a plausible
        // (but wrong) default.
        let garbage = [0_u8; 3];
        let entry = <UnitEntry as redb::Value>::from_bytes(&garbage);
        assert!(entry.is_corrupt());
    }

    #[test]
    fn test_get_unit_errors_on_corrupt_entry() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        // Directly insert an entry shaped like the corruption sentinel to exercise the
        // corrupt-handling paths without needing to smuggle raw garbage bytes through redb's
        // typed table API.
        let write_txn = db.inner.begin_write().unwrap();
        {
            // Create the aliases table too; `get_all_units_with_aliases` below reads it.
            let _ = write_txn.open_table(ALIASES_TABLE).unwrap();
            let mut table = write_txn.open_table(UNITS_TABLE).unwrap();
            table
                .insert(
                    "BAD",
                    UnitEntry {
                        factor: f64::NAN,
                        offset: 0.0,
                        category: UnitCategory::Currency as u8,
                        timestamp: i64::MIN,
                        source: u8::MAX,
                    },
                )
                .unwrap();
            table
                .insert(
                    "USD",
                    UnitEntry {
                        factor: 1.1,
                        offset: 0.0,
                        category: UnitCategory::Currency as u8,
                        timestamp: 100,
                        source: RateSource::Fiat as u8,
                    },
                )
                .unwrap();
        }
        write_txn.commit().unwrap();

        assert!(db.get_unit("BAD").is_err());
        // Non-corrupt entries remain readable.
        assert!(db.get_unit("USD").unwrap().is_some());

        // Iteration-based accessors should silently skip the corrupt entry.
        let units = db.get_category_units(UnitCategory::Currency as u8).unwrap();
        assert!(units.iter().any(|(sym, _)| sym == "USD"));
        assert!(units.iter().all(|(sym, _)| sym != "BAD"));

        let all = db.get_all_units_with_aliases().unwrap();
        assert!(all.contains_key("USD"));
        assert!(!all.contains_key("BAD"));
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
