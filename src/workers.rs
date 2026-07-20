//! Background Tokio workers for periodic fiat/crypto rate refreshes.

use crate::api::{fetch_binance_tickers, fetch_fiat_rates};
use crate::db::Db;
use crate::models::{Config, RateSource};
use anyhow::{Context, Result};
use chrono::Utc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{error, info, warn};

/// Monotonically increasing counter bumped after a successful rate-table write.
///
/// The UI polls this to invalidate the converter unit cache when new symbols appear.
pub type RatesVersion = Arc<AtomicU64>;

/// Creates a shared rates-version counter starting at zero.
#[must_use]
pub fn new_rates_version() -> RatesVersion {
    Arc::new(AtomicU64::new(0))
}

/// Shared config handle: UI publishes updates, workers subscribe via [`watch::Receiver`].
pub type ConfigWatchTx = watch::Sender<Config>;
/// Receiver side of the shared config watch channel.
pub type ConfigWatchRx = watch::Receiver<Config>;

/// Starts the background worker for fiat currency updates.
///
/// Sleeps for the configured interval, but wakes early when config changes so a shortened
/// interval takes effect without waiting out the previous sleep (without forcing an extra fetch).
pub async fn start_fiat_worker(db: Db, mut config_rx: ConfigWatchRx, rates_version: RatesVersion) {
    loop {
        if let Err(err) = update_fiat_rates(&db, &rates_version).await {
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
pub async fn start_crypto_worker(db: Db, mut config_rx: ConfigWatchRx, rates_version: RatesVersion) {
    loop {
        if let Err(err) = update_crypto_rates(&db, &rates_version).await {
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

async fn update_fiat_rates(db: &Db, rates_version: &RatesVersion) -> Result<()> {
    let rates = fetch_fiat_rates()
        .await
        .context("Failed to fetch fiat rates")?;
    let timestamp = Utc::now().timestamp();
    let db = db.clone();
    let batch: Vec<(String, f64)> = rates.into_iter().collect();
    let updated = tokio::task::spawn_blocking(move || {
        db.update_rates_batch(batch, timestamp, RateSource::Fiat)
    })
    .await
    .context("fiat rate DB task join failed")?
    .context("Failed to write fiat rates")?;

    if updated > 0 {
        rates_version.fetch_add(1, Ordering::Relaxed);
    }
    info!(updated, "fiat rates refreshed");
    Ok(())
}

async fn update_crypto_rates(db: &Db, rates_version: &RatesVersion) -> Result<()> {
    let db_for_usdt = db.clone();
    let usdt_factor = tokio::task::spawn_blocking(move || db_for_usdt.get_unit("USDT"))
        .await
        .context("USDT lookup join failed")?
        .context("Failed to read USDT rate")?
        .map_or_else(
            || {
                warn!("USDT rate missing; falling back to 0.92 EUR");
                0.92
            },
            |entry| entry.factor,
        );

    let tickers = fetch_binance_tickers()
        .await
        .context("Failed to fetch crypto tickers")?;
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
        let price_eur = price_usdt * usdt_factor;
        batch.push((symbol.to_string(), price_eur));
    }

    let db = db.clone();
    let updated = tokio::task::spawn_blocking(move || {
        db.update_rates_batch(batch, timestamp, RateSource::Crypto)
    })
    .await
    .context("crypto rate DB task join failed")?
    .context("Failed to write crypto rates")?;

    if updated > 0 {
        rates_version.fetch_add(1, Ordering::Relaxed);
    }
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
}
