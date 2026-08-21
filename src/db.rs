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

/// Key/value metadata (seed version, etc.).
const META_TABLE: TableDefinition<&str, u64> = TableDefinition::new("meta");

/// How much newer a candidate rate row must be than the cached one to overcome a
/// source-priority disadvantage (and how stale a higher-priority row may be before a
/// fresher lower-priority row wins). Keeps e.g. a live fiat feed from being blocked
/// forever by an abandoned crypto row.
const SOURCE_PRIORITY_GRACE_SECS: i64 = 24 * 3600;

/// Staleness-aware rule for whether an incoming rate row replaces the cached entry.
///
/// - Corrupt entries are always replaced.
/// - Static seed rows are only rewritten by reseeding, never by rate batches.
/// - Same source: strictly newer timestamp wins.
/// - Different sources: priority wins only while the cached row is not meaningfully
///   staler than the candidate; beyond [`SOURCE_PRIORITY_GRACE_SECS`] freshness wins.
#[must_use]
fn should_accept_candidate(
    existing: &UnitEntry,
    candidate_source: u8,
    candidate_timestamp: i64,
) -> bool {
    if existing.is_corrupt() {
        return true;
    }
    if existing.source == RateSource::Static as u8 {
        return false;
    }
    match candidate_source.cmp(&existing.source) {
        std::cmp::Ordering::Equal => candidate_timestamp > existing.timestamp,
        std::cmp::Ordering::Greater => {
            // Higher priority wins unless the candidate is itself far staler than
            // the cache (e.g. a replayed old crypto batch over fresh fiat data).
            candidate_timestamp
                >= existing
                    .timestamp
                    .saturating_sub(SOURCE_PRIORITY_GRACE_SECS)
        }
        std::cmp::Ordering::Less => {
            // Lower priority only wins when it is significantly newer than the cache.
            candidate_timestamp
                > existing
                    .timestamp
                    .saturating_add(SOURCE_PRIORITY_GRACE_SECS)
        }
    }
}

/// Bump when [`STATIC_UNITS`] gains rows, factor fixes, or new aliases so existing
/// databases rewrite static entries. `add_unit_static` used to skip symbols that
/// already existed, which left upgrades invisible.
const STATIC_SEED_VERSION: u64 = 1;
const STATIC_SEED_VERSION_KEY: &str = "static_seed_version";

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
            let _ = write_txn
                .open_table(META_TABLE)
                .context("Failed to create meta table")?;
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
    /// [`Db::update_rate`] (a higher-priority source, e.g. Crypto over Fiat, wins unless it is
    /// itself far staler than the cached row; ties on source are broken by the newer
    /// timestamp). All entries share the same `timestamp` and `source`, matching a single
    /// batch API refresh.
    ///
    /// Intended for workers refreshing many symbols at once (e.g. all fiat or all crypto rates)
    /// so that only one write transaction and one table handle are used for the whole batch.
    ///
    /// # Errors
    /// Returns an error if the transaction fails.
    ///
    /// Returns the number of symbols that were actually updated (i.e. that passed the priority
    /// check), which may be less than the number of input rates. Rows with a zero, negative, or
    /// non-finite price are skipped entirely (they cannot yield a usable conversion factor) and
    /// are not counted.
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
                // A non-finite or non-positive price would cache a poison factor
                // (NaN/inf itself, or 1/0 -> inf) and make every conversion routed
                // through this symbol return garbage; drop the row instead.
                if !price.is_finite() || price <= 0.0 {
                    continue;
                }

                let should_update = units_table
                    .get(symbol.as_str())
                    .context("Failed to read existing unit")?
                    .is_none_or(|existing| {
                        let existing_val: UnitEntry = existing.value();
                        should_accept_candidate(&existing_val, source as u8, timestamp)
                    });

                if should_update {
                    // Factor must be "EUR per 1 Unit" so that Base_EUR = Value * Factor.
                    // - Fiat rates are "Units per 1 EUR" (e.g. 1.08 USD/EUR), so Factor = 1/price.
                    // - Crypto rates are already "EUR per 1 Unit" (e.g. 60000 EUR/BTC), so Factor = price.
                    let factor = if source == RateSource::Fiat {
                        1.0 / price
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
    ) -> Result<std::collections::HashMap<String, (Vec<String>, u8)>> {
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

        for result in units_table.iter().context("Failed to iterate units")? {
            let (key, value) = result.context("Failed to read unit row")?;
            let entry = value.value();
            if entry.is_corrupt() {
                continue;
            }
            unit_map.insert(key.value().to_string(), (Vec::new(), entry.category));
        }

        for result in alias_table.iter().context("Failed to iterate aliases")? {
            let (alias, canonical) = result.context("Failed to read alias row")?;
            let canonical_str = canonical.value();
            let alias_str = alias.value();

            if let Some((aliases, _)) = unit_map.get_mut(canonical_str)
                && alias_str.to_lowercase() != canonical_str.to_lowercase()
                && !aliases.contains(&alias_str.to_string())
            {
                aliases.push(alias_str.to_string());
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

    /// Latest fiat and crypto timestamps from cached currency rows (startup seed for the UI).
    ///
    /// # Errors
    /// Returns an error if the read transaction or iteration fails.
    pub fn latest_currency_timestamps(&self) -> Result<(Option<i64>, Option<i64>)> {
        let units = self.get_category_units(UnitCategory::Currency as u8)?;
        let mut fiat: Option<i64> = None;
        let mut crypto: Option<i64> = None;
        for (_, entry) in units {
            if entry.timestamp <= 0 {
                continue;
            }
            if entry.source == RateSource::Fiat as u8 {
                fiat = Some(fiat.map_or(entry.timestamp, |prev| prev.max(entry.timestamp)));
            } else if entry.source == RateSource::Crypto as u8 {
                crypto = Some(crypto.map_or(entry.timestamp, |prev| prev.max(entry.timestamp)));
            }
        }
        Ok((fiat, crypto))
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

    /// Initializes (or upgrades) static units and aliases.
    ///
    /// No-ops when [`STATIC_SEED_VERSION`] is already stored, so launch stays cheap.
    /// On a missing or older version, every [`STATIC_UNITS`] row is rewritten.
    ///
    /// # Errors
    /// Returns an error if any transaction fails.
    pub fn init_static_units(&self) -> Result<()> {
        let write_txn = self
            .inner
            .begin_write()
            .context("Failed to begin write transaction")?;
        let needs_seed = {
            let meta = write_txn
                .open_table(META_TABLE)
                .context("Failed to open meta table")?;
            meta.get(STATIC_SEED_VERSION_KEY)
                .context("Failed to read static seed version")?
                .map_or(0, |value| value.value())
                < STATIC_SEED_VERSION
        };
        if !needs_seed {
            return Ok(());
        }

        {
            let mut units = write_txn
                .open_table(UNITS_TABLE)
                .context("Failed to open units table")?;
            let mut aliases = write_txn
                .open_table(ALIASES_TABLE)
                .context("Failed to open aliases table")?;
            seed_static_units(&mut units, &mut aliases)?;
        }
        {
            let mut meta = write_txn
                .open_table(META_TABLE)
                .context("Failed to open meta table")?;
            meta.insert(STATIC_SEED_VERSION_KEY, STATIC_SEED_VERSION)
                .context("Failed to store static seed version")?;
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

/// Static length/weight/temperature/time/volume/area/speed/data units seeded on first launch.
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
    StaticUnit {
        symbol: "L",
        category: UnitCategory::Volume,
        factor: 1.0,
        offset: 0.0,
        aliases: &["l", "liter", "liters", "litre", "litres"],
    },
    StaticUnit {
        symbol: "mL",
        category: UnitCategory::Volume,
        factor: 0.001,
        offset: 0.0,
        aliases: &[
            "ml",
            "milliliter",
            "milliliters",
            "millilitre",
            "millilitres",
        ],
    },
    StaticUnit {
        symbol: "gal",
        category: UnitCategory::Volume,
        factor: 3.785_411_784,
        offset: 0.0,
        aliases: &["gallon", "gallons"],
    },
    StaticUnit {
        symbol: "fl oz",
        category: UnitCategory::Volume,
        factor: 0.029_573_529_562_5,
        offset: 0.0,
        aliases: &["floz", "fluid ounce", "fluid ounces"],
    },
    StaticUnit {
        symbol: "m2",
        category: UnitCategory::Area,
        factor: 1.0,
        offset: 0.0,
        aliases: &["m²", "sqm", "square meter", "square meters"],
    },
    StaticUnit {
        symbol: "km2",
        category: UnitCategory::Area,
        factor: 1_000_000.0,
        offset: 0.0,
        aliases: &["km²", "square kilometer", "square kilometers"],
    },
    StaticUnit {
        symbol: "ha",
        category: UnitCategory::Area,
        factor: 10_000.0,
        offset: 0.0,
        aliases: &["hectare", "hectares"],
    },
    StaticUnit {
        symbol: "acre",
        category: UnitCategory::Area,
        factor: 4_046.856_422_4,
        offset: 0.0,
        aliases: &["acres", "ac"],
    },
    StaticUnit {
        symbol: "ft2",
        category: UnitCategory::Area,
        factor: 0.092_903_04,
        offset: 0.0,
        aliases: &["ft²", "sqft", "square foot", "square feet"],
    },
    StaticUnit {
        symbol: "m/s",
        category: UnitCategory::Speed,
        factor: 1.0,
        offset: 0.0,
        aliases: &["mps", "meter per second", "meters per second"],
    },
    StaticUnit {
        symbol: "km/h",
        category: UnitCategory::Speed,
        factor: 1.0 / 3.6,
        offset: 0.0,
        aliases: &["kph", "kmh", "kilometer per hour", "kilometers per hour"],
    },
    StaticUnit {
        symbol: "mph",
        category: UnitCategory::Speed,
        factor: 0.447_04,
        offset: 0.0,
        aliases: &["mi/h", "mile per hour", "miles per hour"],
    },
    StaticUnit {
        symbol: "kn",
        category: UnitCategory::Speed,
        factor: 0.514_444,
        offset: 0.0,
        aliases: &["kt", "knot", "knots"],
    },
    StaticUnit {
        symbol: "B",
        category: UnitCategory::Data,
        factor: 1.0,
        offset: 0.0,
        aliases: &["byte", "bytes"],
    },
    StaticUnit {
        symbol: "KB",
        category: UnitCategory::Data,
        factor: 1000.0,
        offset: 0.0,
        aliases: &["kilobyte", "kilobytes"],
    },
    StaticUnit {
        symbol: "MB",
        category: UnitCategory::Data,
        factor: 1_000_000.0,
        offset: 0.0,
        aliases: &["megabyte", "megabytes"],
    },
    StaticUnit {
        symbol: "GB",
        category: UnitCategory::Data,
        factor: 1_000_000_000.0,
        offset: 0.0,
        aliases: &["gigabyte", "gigabytes"],
    },
    StaticUnit {
        symbol: "KiB",
        category: UnitCategory::Data,
        factor: 1024.0,
        offset: 0.0,
        aliases: &["kibibyte", "kibibytes"],
    },
    StaticUnit {
        symbol: "MiB",
        category: UnitCategory::Data,
        factor: 1_048_576.0,
        offset: 0.0,
        aliases: &["mebibyte", "mebibytes"],
    },
    StaticUnit {
        symbol: "GiB",
        category: UnitCategory::Data,
        factor: 1_073_741_824.0,
        offset: 0.0,
        aliases: &["gibibyte", "gibibytes"],
    },
    StaticUnit {
        symbol: "Pa",
        category: UnitCategory::Pressure,
        factor: 1.0,
        offset: 0.0,
        aliases: &["pascal", "pascals"],
    },
    StaticUnit {
        symbol: "kPa",
        category: UnitCategory::Pressure,
        factor: 1000.0,
        offset: 0.0,
        aliases: &["kilopascal", "kilopascals"],
    },
    StaticUnit {
        symbol: "bar",
        category: UnitCategory::Pressure,
        factor: 100_000.0,
        offset: 0.0,
        aliases: &["bars"],
    },
    StaticUnit {
        symbol: "atm",
        category: UnitCategory::Pressure,
        factor: 101_325.0,
        offset: 0.0,
        aliases: &["atmosphere", "atmospheres"],
    },
    StaticUnit {
        symbol: "psi",
        category: UnitCategory::Pressure,
        factor: 6_894.757_293_168,
        offset: 0.0,
        aliases: &["psia"],
    },
    StaticUnit {
        symbol: "J",
        category: UnitCategory::Energy,
        factor: 1.0,
        offset: 0.0,
        aliases: &["joule", "joules"],
    },
    StaticUnit {
        symbol: "kJ",
        category: UnitCategory::Energy,
        factor: 1000.0,
        offset: 0.0,
        aliases: &["kilojoule", "kilojoules"],
    },
    StaticUnit {
        symbol: "kcal",
        category: UnitCategory::Energy,
        factor: 4184.0,
        offset: 0.0,
        aliases: &["kilocalorie", "kilocalories", "Cal"],
    },
    StaticUnit {
        symbol: "Wh",
        category: UnitCategory::Energy,
        factor: 3600.0,
        offset: 0.0,
        aliases: &["watt-hour", "watt hours"],
    },
    StaticUnit {
        symbol: "kWh",
        category: UnitCategory::Energy,
        factor: 3_600_000.0,
        offset: 0.0,
        aliases: &["kilowatt-hour", "kilowatt hours"],
    },
    StaticUnit {
        symbol: "W",
        category: UnitCategory::Power,
        factor: 1.0,
        offset: 0.0,
        aliases: &["watt", "watts"],
    },
    StaticUnit {
        symbol: "kW",
        category: UnitCategory::Power,
        factor: 1000.0,
        offset: 0.0,
        aliases: &["kilowatt", "kilowatts"],
    },
    StaticUnit {
        symbol: "hp",
        category: UnitCategory::Power,
        factor: 745.7,
        offset: 0.0,
        aliases: &["horsepower"],
    },
    StaticUnit {
        symbol: "N",
        category: UnitCategory::Force,
        factor: 1.0,
        offset: 0.0,
        aliases: &["newton", "newtons"],
    },
    StaticUnit {
        symbol: "lbf",
        category: UnitCategory::Force,
        factor: 4.448_221_615_260_5,
        offset: 0.0,
        aliases: &["pound-force"],
    },
    StaticUnit {
        symbol: "rad",
        category: UnitCategory::Angle,
        factor: 1.0,
        offset: 0.0,
        aliases: &["radian", "radians"],
    },
    StaticUnit {
        symbol: "deg",
        category: UnitCategory::Angle,
        factor: std::f64::consts::PI / 180.0,
        offset: 0.0,
        aliases: &["degree", "degrees", "°"],
    },
    StaticUnit {
        symbol: "Hz",
        category: UnitCategory::Frequency,
        factor: 1.0,
        offset: 0.0,
        aliases: &["hertz"],
    },
    StaticUnit {
        symbol: "kHz",
        category: UnitCategory::Frequency,
        factor: 1000.0,
        offset: 0.0,
        aliases: &["kilohertz"],
    },
    StaticUnit {
        symbol: "MHz",
        category: UnitCategory::Frequency,
        factor: 1_000_000.0,
        offset: 0.0,
        aliases: &["megahertz"],
    },
    StaticUnit {
        symbol: "GHz",
        category: UnitCategory::Frequency,
        factor: 1_000_000_000.0,
        offset: 0.0,
        aliases: &["gigahertz"],
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

/// Writes a static unit and its aliases. Called only when the seed version is
/// behind, so overwriting existing static rows is the upgrade path.
fn add_unit_static(
    units: &mut redb::Table<&str, UnitEntry>,
    aliases: &mut redb::Table<&str, &str>,
    unit: &StaticUnit,
) -> Result<()> {
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
    fn update_rates_batch_should_skip_zero_price_rows() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        // A 0.0 price would cache factor 0.0 (or 1/0) and poison conversions
        // routed through that symbol; it must not be inserted or counted.
        let rates = [
            ("USD".to_string(), 1.08),
            ("ZERO".to_string(), 0.0),
            ("PLN".to_string(), 4.0),
        ];
        let updated = db
            .update_rates_batch(rates, 1000, RateSource::Fiat)
            .unwrap();
        assert_eq!(updated, 2);

        assert!(db.get_unit("USD").unwrap().is_some());
        assert!(db.get_unit("PLN").unwrap().is_some());
        assert!(
            db.get_unit("ZERO").unwrap().is_none(),
            "zero-price row must not be cached"
        );
    }

    #[test]
    fn update_rates_batch_should_skip_negative_and_nonfinite_prices() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        let rates = [
            ("NEG".to_string(), -4.0),
            ("NAN".to_string(), f64::NAN),
            ("INF".to_string(), f64::INFINITY),
            ("OK".to_string(), 2.5),
        ];
        let updated = db
            .update_rates_batch(rates, 1000, RateSource::Fiat)
            .unwrap();
        assert_eq!(updated, 1);

        let ok = db.get_unit("OK").unwrap().unwrap();
        assert!((ok.factor - 0.4).abs() < f64::EPSILON);
        assert!(db.get_unit("NEG").unwrap().is_none());
        assert!(db.get_unit("NAN").unwrap().is_none());
        assert!(db.get_unit("INF").unwrap().is_none());
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
    fn latest_currency_timestamps_should_return_newest_fiat_and_crypto() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        db.update_rate("USD", 1.08, 1000, RateSource::Fiat).unwrap();
        db.update_rate("PLN", 4.0, 2000, RateSource::Fiat).unwrap();
        db.update_rate("BTC", 50_000.0, 1500, RateSource::Crypto)
            .unwrap();
        db.update_rate("ETH", 3_000.0, 1800, RateSource::Crypto)
            .unwrap();

        let (fiat, crypto) = db.latest_currency_timestamps().unwrap();
        assert_eq!(fiat, Some(2000));
        assert_eq!(crypto, Some(1800));
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
    fn should_accept_candidate_replaces_missing_or_corrupt_rows() {
        assert!(should_accept_candidate(
            &UnitEntry::corrupt_sentinel(),
            RateSource::Fiat as u8,
            0
        ));
    }

    #[test]
    fn should_accept_candidate_never_overwrites_static_seed() {
        let static_row = UnitEntry {
            factor: 1000.0,
            offset: 0.0,
            category: UnitCategory::Length as u8,
            timestamp: 0,
            source: RateSource::Static as u8,
        };
        assert!(!should_accept_candidate(
            &static_row,
            RateSource::Crypto as u8,
            i64::MAX
        ));
    }

    #[test]
    fn should_accept_candidate_ties_broken_by_newer_timestamp() {
        let fiat = UnitEntry {
            factor: 0.25,
            offset: 0.0,
            category: UnitCategory::Currency as u8,
            timestamp: 1000,
            source: RateSource::Fiat as u8,
        };
        assert!(should_accept_candidate(&fiat, RateSource::Fiat as u8, 1001));
        assert!(!should_accept_candidate(
            &fiat,
            RateSource::Fiat as u8,
            1000
        ));
        assert!(!should_accept_candidate(&fiat, RateSource::Fiat as u8, 999));
    }

    #[test]
    fn should_accept_candidate_higher_priority_loses_when_far_staler() {
        // Fresh fiat cache at T; a crypto candidate more than GRACE staler must lose.
        let fiat_now = UnitEntry {
            factor: 0.25,
            offset: 0.0,
            category: UnitCategory::Currency as u8,
            timestamp: 100_000,
            source: RateSource::Fiat as u8,
        };
        assert!(!should_accept_candidate(
            &fiat_now,
            RateSource::Crypto as u8,
            100_000 - SOURCE_PRIORITY_GRACE_SECS - 1
        ));
        // Within the grace window (or newer), higher priority wins as before.
        assert!(should_accept_candidate(
            &fiat_now,
            RateSource::Crypto as u8,
            100_000 - SOURCE_PRIORITY_GRACE_SECS + 1
        ));
        assert!(should_accept_candidate(
            &fiat_now,
            RateSource::Crypto as u8,
            200_000
        ));
    }

    #[test]
    fn should_accept_candidate_lower_priority_wins_only_when_significantly_newer() {
        // Stale crypto cache that has not refreshed in days.
        let stale_crypto = UnitEntry {
            factor: 50_000.0,
            offset: 0.0,
            category: UnitCategory::Currency as u8,
            timestamp: 1000,
            source: RateSource::Crypto as u8,
        };
        // Slightly newer fiat does NOT beat the higher-priority crypto row...
        assert!(!should_accept_candidate(
            &stale_crypto,
            RateSource::Fiat as u8,
            1000 + SOURCE_PRIORITY_GRACE_SECS - 1
        ));
        // ...but a fiat row beyond the grace window finally wins.
        assert!(should_accept_candidate(
            &stale_crypto,
            RateSource::Fiat as u8,
            1000 + SOURCE_PRIORITY_GRACE_SECS + 1
        ));
    }

    #[test]
    fn batch_newer_fiat_beats_stale_crypto_after_grace_window() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        // Crypto stopped refreshing after ts 10_000.
        db.update_rate("BTC", 50_000.0, 10_000, RateSource::Crypto)
            .unwrap();

        // Next daily fiat refresh is still inside the grace window: skipped.
        let updated = db
            .update_rates_batch(
                [("BTC".to_string(), 49_000.0)],
                10_000 + 3600,
                RateSource::Fiat,
            )
            .unwrap();
        assert_eq!(updated, 0);
        let btc = db.get_unit("BTC").unwrap().unwrap();
        assert_eq!(btc.source, RateSource::Crypto as u8);

        // Once the crypto row is older than the grace window, fiat wins.
        let updated = db.update_rate(
            "BTC",
            48_000.0,
            10_000 + SOURCE_PRIORITY_GRACE_SECS + 1,
            RateSource::Fiat,
        );
        assert!(updated.is_ok());
        let btc = db.get_unit("BTC").unwrap().unwrap();
        assert_eq!(btc.source, RateSource::Fiat as u8);
        assert!((btc.factor - (1.0 / 48_000.0)).abs() < f64::EPSILON);
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

        let litre = db.get_unit("L").unwrap().unwrap();
        assert_eq!(litre.category, UnitCategory::Volume as u8);
    }

    #[test]
    fn init_static_units_should_rewrite_stale_static_rows_when_seed_unversioned() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        db.update_unit("m", 99.0, 0.0, UnitCategory::Length, RateSource::Static)
            .unwrap();
        assert!((db.get_unit("m").unwrap().unwrap().factor - 99.0).abs() < f64::EPSILON);

        db.init_static_units().unwrap();

        let metres = db.get_unit("m").unwrap().unwrap();
        assert!((metres.factor - 1.0).abs() < f64::EPSILON);
        assert_eq!(db.resolve_symbol("meters").unwrap(), "m");
        assert!(db.get_unit("L").unwrap().is_some());
    }

    #[test]
    fn init_static_units_should_skip_rewrite_when_seed_version_current() {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db {
            inner: Arc::new(db_inner),
        };

        db.init_static_units().unwrap();
        db.update_unit("m", 99.0, 0.0, UnitCategory::Length, RateSource::Static)
            .unwrap();

        db.init_static_units().unwrap();

        let metres = db.get_unit("m").unwrap().unwrap();
        assert!((metres.factor - 99.0).abs() < f64::EPSILON);
    }
}
