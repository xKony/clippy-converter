use crate::api::{fetch_binance_tickers, fetch_fiat_rates};
use crate::db::Db;
use crate::models::{Config, RateSource};
use anyhow::{Context, Result};
use chrono::Utc;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::time::sleep;

/// Shared, thread-safe configuration handle.
pub type SharedConfig = Arc<RwLock<Config>>;

/// Starts the background worker for fiat currency updates.
/// Reads the interval from the shared config on each iteration
/// so that runtime changes are picked up immediately.
pub async fn start_fiat_worker(db: Db, config: SharedConfig) {
    loop {
        let _ = update_fiat_rates(&db).await;
        let interval_mins = config
            .read()
            .map_or(1440, |c| c.fiat_update_interval_mins);
        sleep(Duration::from_secs(interval_mins * 60)).await;
    }
}

/// Starts the background worker for cryptocurrency updates.
/// Reads the interval from the shared config on each iteration
/// so that runtime changes are picked up immediately.
pub async fn start_crypto_worker(db: Db, config: SharedConfig) {
    loop {
        let _ = update_crypto_rates(&db).await;
        let interval_mins = config
            .read()
            .map_or(60, |c| c.crypto_update_interval_mins);
        sleep(Duration::from_secs(interval_mins * 60)).await;
    }
}

async fn update_fiat_rates(db: &Db) -> Result<()> {
    let rates = fetch_fiat_rates()
        .await
        .context("Failed to fetch fiat rates")?;
    let timestamp = Utc::now().timestamp();

    for (symbol, price) in rates {
        db.update_rate(&symbol, price, timestamp, RateSource::Fiat)?;
    }
    Ok(())
}

async fn update_crypto_rates(db: &Db) -> Result<()> {
    // 1. Get the conversion factor for USDT to EUR (EUR per 1 USDT)
    // We prefer the normalized factor from UNITS_TABLE which is always "EUR per Unit".
    let usdt_factor = db.get_unit("USDT")?.map_or(0.92, |entry| entry.factor);

    let tickers = fetch_binance_tickers()
        .await
        .context("Failed to fetch crypto tickers")?;
    let timestamp = Utc::now().timestamp();

    for ticker in tickers {
        // We only care about USDT pairs for now
        if let Some(symbol) = ticker.symbol.strip_suffix("USDT")
            && let Ok(price_usdt) = ticker.price.parse::<f64>()
        {
            // price_usdt = USDT / 1 Unit (e.g. 65000 USDT / 1 BTC)
            // usdt_factor = EUR / 1 USDT (e.g. 0.92 EUR / 1 USDT)
            // price_eur = price_usdt * usdt_factor (EUR / 1 Unit)
            let price_eur = price_usdt * usdt_factor;

            db.update_rate(symbol, price_eur, timestamp, RateSource::Crypto)?;
        }
    }
    Ok(())
}
