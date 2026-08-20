//! Background Tokio workers for periodic fiat/crypto rate refreshes.

use crate::api::{fetch_binance_tickers, fetch_fiat_rates};
use crate::db::Db;
use crate::models::{Config, RateSource};
use anyhow::{Context, Result};
use chrono::Utc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::watch;
use tracing::{error, info, warn};

/// Shared rate-refresh bookkeeping: version for cache invalidation plus last-success age.
#[derive(Clone)]
pub struct RatesStatus {
    inner: Arc<RatesStatusInner>,
}

struct RatesStatusInner {
    version: AtomicU64,
    last_fiat_unix: AtomicI64,
    last_crypto_unix: AtomicI64,
    last_fiat_failed: AtomicBool,
    last_crypto_failed: AtomicBool,
}

/// Point-in-time view of [`RatesStatus`] for the UI (no atomics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatesSnapshot {
    pub last_fiat_unix: Option<i64>,
    pub last_crypto_unix: Option<i64>,
    pub last_fiat_failed: bool,
    pub last_crypto_failed: bool,
}

/// Fiat older than this is shown as stale (daily refresh, so missing a day+).
const FIAT_STALE_SECS: i64 = 48 * 3600;
/// Crypto older than this is shown as stale (hourly refresh).
const CRYPTO_STALE_SECS: i64 = 6 * 3600;

impl RatesStatus {
    /// Creates a shared status object starting with no known refresh times.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RatesStatusInner {
                version: AtomicU64::new(0),
                last_fiat_unix: AtomicI64::new(0),
                last_crypto_unix: AtomicI64::new(0),
                last_fiat_failed: AtomicBool::new(false),
                last_crypto_failed: AtomicBool::new(false),
            }),
        }
    }

    /// Seeds last-success timestamps from the on-disk cache (startup, not UI-thread).
    pub fn seed(&self, fiat_unix: Option<i64>, crypto_unix: Option<i64>) {
        if let Some(ts) = fiat_unix.filter(|ts| *ts > 0) {
            self.inner.last_fiat_unix.store(ts, Ordering::Relaxed);
        }
        if let Some(ts) = crypto_unix.filter(|ts| *ts > 0) {
            self.inner.last_crypto_unix.store(ts, Ordering::Relaxed);
        }
    }

    /// Monotonic counter the UI polls to invalidate the converter unit cache.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.inner.version.load(Ordering::Relaxed)
    }

    /// Copies atomics for display. Cheap enough to call from the UI frame path.
    #[must_use]
    pub fn snapshot(&self) -> RatesSnapshot {
        RatesSnapshot {
            last_fiat_unix: nonzero_unix(self.inner.last_fiat_unix.load(Ordering::Relaxed)),
            last_crypto_unix: nonzero_unix(self.inner.last_crypto_unix.load(Ordering::Relaxed)),
            last_fiat_failed: self.inner.last_fiat_failed.load(Ordering::Relaxed),
            last_crypto_failed: self.inner.last_crypto_failed.load(Ordering::Relaxed),
        }
    }

    fn record_fiat_success(&self, unix: i64) {
        self.inner.last_fiat_unix.store(unix, Ordering::Relaxed);
        self.inner.last_fiat_failed.store(false, Ordering::Relaxed);
        self.inner.version.fetch_add(1, Ordering::Relaxed);
    }

    fn record_crypto_success(&self, unix: i64) {
        self.inner.last_crypto_unix.store(unix, Ordering::Relaxed);
        self.inner.last_crypto_failed.store(false, Ordering::Relaxed);
        self.inner.version.fetch_add(1, Ordering::Relaxed);
    }

    fn record_fiat_failure(&self) {
        self.inner.last_fiat_failed.store(true, Ordering::Relaxed);
    }

    fn record_crypto_failure(&self) {
        self.inner.last_crypto_failed.store(true, Ordering::Relaxed);
    }
}

impl Default for RatesStatus {
    fn default() -> Self {
        Self::new()
    }
}

fn nonzero_unix(value: i64) -> Option<i64> {
    (value > 0).then_some(value)
}

impl RatesSnapshot {
    /// One-line popup footer, e.g. `Rates: fiat 3h ago · crypto 12m ago`.
    #[must_use]
    pub fn summary_line(self, now_unix: i64) -> String {
        let fiat = format_age(self.last_fiat_unix, now_unix, self.last_fiat_failed);
        let crypto = format_age(self.last_crypto_unix, now_unix, self.last_crypto_failed);
        format!("Rates: fiat {fiat} · crypto {crypto}")
    }

    /// True when cached rates are old enough (or a refresh just failed) that the UI should warn.
    #[must_use]
    pub fn is_stale(self, now_unix: i64) -> bool {
        let fiat_stale = self.last_fiat_failed
            || self
                .last_fiat_unix
                .is_some_and(|ts| now_unix.saturating_sub(ts) > FIAT_STALE_SECS);
        let crypto_stale = self.last_crypto_failed
            || self
                .last_crypto_unix
                .is_some_and(|ts| now_unix.saturating_sub(ts) > CRYPTO_STALE_SECS);
        fiat_stale || crypto_stale
    }
}

fn format_age(unix: Option<i64>, now_unix: i64, failed: bool) -> String {
    let age = unix.map_or_else(
        || "never".to_string(),
        |ts| {
            let secs = now_unix.saturating_sub(ts).max(0);
            if secs < 90 {
                "just now".to_string()
            } else if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86_400 {
                format!("{}h ago", secs / 3600)
            } else {
                format!("{}d ago", secs / 86_400)
            }
        },
    );
    if failed {
        format!("{age} (refresh failed)")
    } else {
        age
    }
}

/// Shared config handle: UI publishes updates, workers subscribe via [`watch::Receiver`].
pub type ConfigWatchTx = watch::Sender<Config>;
/// Receiver side of the shared config watch channel.
pub type ConfigWatchRx = watch::Receiver<Config>;

/// Starts the background worker for fiat currency updates.
///
/// Sleeps for the configured interval, but wakes early when config changes so a shortened
/// interval takes effect without waiting out the previous sleep (without forcing an extra fetch).
pub async fn start_fiat_worker(db: Db, mut config_rx: ConfigWatchRx, rates: RatesStatus) {
    loop {
        if let Err(err) = update_fiat_rates(&db, &rates).await {
            error!(error = %err, "fiat rate refresh failed");
        }
        if !wait_for_interval(&mut config_rx, |c| c.fiat_update_interval_mins).await {
            break;
        }
    }
}

/// Starts the background worker for cryptocurrency updates.
///
/// Sleeps for the configured interval, but wakes early when config changes so a shortened
/// interval takes effect without waiting out the previous sleep (without forcing an extra fetch).
pub async fn start_crypto_worker(db: Db, mut config_rx: ConfigWatchRx, rates: RatesStatus) {
    loop {
        if let Err(err) = update_crypto_rates(&db, &rates).await {
            error!(error = %err, "crypto rate refresh failed");
        }
        if !wait_for_interval(&mut config_rx, |c| c.crypto_update_interval_mins).await {
            break;
        }
    }
}

/// Waits until the configured interval elapses, restarting the timer if config changes.
///
/// Returns `false` when the config sender is dropped (worker should shut down).
async fn wait_for_interval(config_rx: &mut ConfigWatchRx, interval_mins: fn(&Config) -> u64) -> bool {
    loop {
        let mins = interval_mins(&config_rx.borrow()).max(1);
        let sleep = tokio::time::sleep(Duration::from_secs(mins.saturating_mul(60)));
        tokio::pin!(sleep);

        tokio::select! {
            () = &mut sleep => {
                return true;
            }
            result = config_rx.changed() => {
                if result.is_err() {
                    return false;
                }
                info!("rate worker woke early due to config change; restarting interval timer");
            }
        }
    }
}

async fn update_fiat_rates(db: &Db, rates: &RatesStatus) -> Result<()> {
    let fetched = match fetch_fiat_rates().await {
        Ok(fetched) => fetched,
        Err(err) => {
            rates.record_fiat_failure();
            return Err(err).context("Failed to fetch fiat rates");
        }
    };
    let timestamp = Utc::now().timestamp();
    let db = db.clone();
    let batch: Vec<(String, f64)> = fetched.into_iter().collect();
    let updated = tokio::task::spawn_blocking(move || {
        db.update_rates_batch(batch, timestamp, RateSource::Fiat)
    })
    .await
    .context("fiat rate DB task join failed")?
    .context("Failed to write fiat rates")?;

    rates.record_fiat_success(timestamp);
    info!(updated, "fiat rates refreshed");
    Ok(())
}

async fn update_crypto_rates(db: &Db, rates: &RatesStatus) -> Result<()> {
    let db_for_usdt = db.clone();
    let usdt_factor = tokio::task::spawn_blocking(move || db_for_usdt.get_unit("USDT"))
        .await
        .context("USDT lookup join failed")?
        .context("Failed to read USDT rate")?;
    let Some(usdt_entry) = usdt_factor else {
        warn!("USDT rate missing; skipping crypto refresh until fiat lands");
        return Ok(());
    };

    let tickers = match fetch_binance_tickers().await {
        Ok(tickers) => tickers,
        Err(err) => {
            rates.record_crypto_failure();
            return Err(err).context("Failed to fetch crypto tickers");
        }
    };
    let timestamp = Utc::now().timestamp();

    let mut batch = Vec::with_capacity(tickers.len());
    for ticker in tickers {
        let Some(symbol) = ticker.symbol.strip_suffix("USDT") else {
            continue;
        };
        let Ok(price_usdt) = ticker.price.parse::<f64>() else {
            continue;
        };
        // price_usdt = USDT / 1 Unit; usdt_factor = EUR / 1 USDT → EUR / 1 Unit
        let price_eur = price_usdt * usdt_entry.factor;
        batch.push((symbol.to_string(), price_eur));
    }

    let db = db.clone();
    let updated = tokio::task::spawn_blocking(move || {
        db.update_rates_batch(batch, timestamp, RateSource::Crypto)
    })
    .await
    .context("crypto rate DB task join failed")?
    .context("Failed to write crypto rates")?;

    rates.record_crypto_success(timestamp);
    info!(updated, "crypto rates refreshed");
    Ok(())
}

/// Pure helper used by tests: convert USDT-quoted prices to EUR using a USDT→EUR factor.
#[cfg(test)]
fn crypto_prices_to_eur(tickers: &[(String, f64)], usdt_factor: f64) -> Vec<(String, f64)> {
    tickers
        .iter()
        .filter_map(|(symbol, price_usdt)| {
            let base = symbol.strip_suffix("USDT")?;
            Some((base.to_string(), price_usdt * usdt_factor))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;

    #[test]
    fn crypto_prices_to_eur_strips_usdt_and_scales() {
        let tickers = vec![
            ("BTCUSDT".to_string(), 50_000.0),
            ("ETHBTC".to_string(), 0.05),
            ("ETHUSDT".to_string(), 3_000.0),
        ];
        let converted = crypto_prices_to_eur(&tickers, 0.9);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0].0, "BTC");
        assert!((converted[0].1 - 45_000.0).abs() < f64::EPSILON);
        assert_eq!(converted[1].0, "ETH");
        assert!((converted[1].1 - 2_700.0).abs() < f64::EPSILON);
    }

    #[test]
    fn format_age_should_use_relative_labels() {
        assert_eq!(format_age(None, 1000, false), "never");
        assert_eq!(format_age(Some(1000), 1030, false), "just now");
        assert_eq!(format_age(Some(1000), 1000 + 10 * 60, false), "10m ago");
        assert_eq!(format_age(Some(1000), 1000 + 3 * 3600, false), "3h ago");
        assert_eq!(format_age(Some(1000), 1000 + 2 * 86_400, false), "2d ago");
        assert_eq!(
            format_age(Some(1000), 1030, true),
            "just now (refresh failed)"
        );
    }

    #[test]
    fn snapshot_should_warn_when_fiat_is_two_days_old() {
        let snap = RatesSnapshot {
            last_fiat_unix: Some(1),
            last_crypto_unix: Some(1 + 60),
            last_fiat_failed: false,
            last_crypto_failed: false,
        };
        assert!(!snap.is_stale(1 + 3600));
        assert!(snap.is_stale(1 + FIAT_STALE_SECS + 1));
        assert_eq!(
            snap.summary_line(1 + 3600),
            "Rates: fiat 1h ago · crypto 59m ago"
        );
    }

    #[test]
    fn snapshot_should_warn_when_last_refresh_failed() {
        let snap = RatesSnapshot {
            last_fiat_unix: Some(1000),
            last_crypto_unix: None,
            last_fiat_failed: true,
            last_crypto_failed: false,
        };
        assert!(snap.is_stale(1060));
    }
}
