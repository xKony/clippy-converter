use anyhow::{Context, Result};
use directories::ProjectDirs;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Schema for the rates table: Symbol -> (Price, Timestamp, `SourceID`)
const RATES_TABLE: TableDefinition<&str, RateEntry> = TableDefinition::new("rates");

/// Represents the source of a currency rate.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum RateSource {
    /// Daily fallback from fiat API.
    Fiat = 0,
    /// High-frequency update from crypto API.
    Crypto = 1,
}

/// A single rate entry stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RateEntry {
    /// The price relative to the internal base (EUR).
    pub price: f64,
    /// Unix timestamp of the update.
    pub timestamp: i64,
    /// The source identifier.
    pub source: u8,
}

// Implement redb::Value for RateEntry using bincode serialization.
impl redb::Value for RateEntry {
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
            price: 0.0,
            timestamp: 0,
            source: 0,
        })
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a> {
        bincode::serialize(value).unwrap_or_default()
    }

    fn type_name() -> redb::TypeName {
        redb::TypeName::new("RateEntry")
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
                .open_table(RATES_TABLE)
                .context("Failed to create rates table")?;
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
            let mut table = write_txn
                .open_table(RATES_TABLE)
                .context("Failed to open rates table")?;

            let should_update = table
                .get(symbol)
                .context("Failed to read existing rate")?
                .is_none_or(|existing| {
                    let existing_val: RateEntry = existing.value();
                    (source as u8 > existing_val.source)
                        || (source as u8 == existing_val.source
                            && timestamp > existing_val.timestamp)
                });

            if should_update {
                table
                    .insert(
                        symbol,
                        RateEntry {
                            price,
                            timestamp,
                            source: source as u8,
                        },
                    )
                    .context("Failed to insert rate into database")?;
            }
        }
        write_txn
            .commit()
            .context("Failed to commit write transaction")?;
        Ok(())
    }

    /// Retrieves a rate for a given symbol.
    ///
    /// # Errors
    /// Returns an error if the read transaction fails.
    pub fn get_rate(&self, symbol: &str) -> Result<Option<RateEntry>> {
        let read_txn = self
            .inner
            .begin_read()
            .context("Failed to begin read transaction")?;
        let table = read_txn
            .open_table(RATES_TABLE)
            .context("Failed to open rates table")?;
        let result = table.get(symbol).context("Failed to query symbol")?;
        Ok(result.map(|r| r.value()))
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
            .open_table(RATES_TABLE)
            .context("Failed to open rates table")?;
        let mut symbols = Vec::new();
        for result in table.iter().context("Failed to iterate table")? {
            let (key, _) = result.context("Failed to read row")?;
            symbols.push(key.value().to_string());
        }
        Ok(symbols)
    }
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
        db.update_rate("BTC", 50000.0, 1000, RateSource::Fiat)
            .unwrap();
        let entry = db.get_rate("BTC").unwrap().unwrap();
        assert_eq!(entry.source, RateSource::Fiat as u8);

        // 2. Insert Crypto rate (Higher priority, even if older - though normally it wouldn't be)
        db.update_rate("BTC", 51000.0, 900, RateSource::Crypto)
            .unwrap();
        let entry = db.get_rate("BTC").unwrap().unwrap();
        assert_eq!(entry.source, RateSource::Crypto as u8);
        assert_eq!(entry.price, 51000.0);

        // 3. Try to overwrite Crypto with older Fiat (Should fail)
        db.update_rate("BTC", 49000.0, 1100, RateSource::Fiat)
            .unwrap();
        let entry = db.get_rate("BTC").unwrap().unwrap();
        assert_eq!(entry.source, RateSource::Crypto as u8);
    }
}
