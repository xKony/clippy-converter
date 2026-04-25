use crate::api::{fetch_binance_tickers, fetch_fiat_rates};
use crate::db::Db;
use crate::models::{Config, RateSource};
use anyhow::{Context, Result};
use chrono::Utc;
use std::time::Duration;
use tokio::time::sleep;

/// Starts the background worker for fiat currency updates.
pub async fn start_fiat_worker(db: Db, config: Config) {
    loop {
        if let Err(e) = update_fiat_rates(&db).await {
            eprintln!("Fiat worker error: {e:?}");
        }
        sleep(Duration::from_secs(config.fiat_update_interval_mins * 60)).await;
    }
}

/// Starts the background worker for cryptocurrency updates.
pub async fn start_crypto_worker(db: Db, config: Config) {
    loop {
        if let Err(e) = update_crypto_rates(&db).await {
            eprintln!("Crypto worker error: {e:?}");
        }
        sleep(Duration::from_secs(config.crypto_update_interval_mins * 60)).await;
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
    println!("Fiat rates updated successfully.");
    Ok(())
}

async fn update_crypto_rates(db: &Db) -> Result<()> {
    // 1. Get the conversion factor for USDT to EUR (EUR per 1 USDT)
    // We prefer the normalized factor from UNITS_TABLE which is always "EUR per Unit".
    let usdt_factor = db
        .get_unit("USDT")?
        .map_or(0.92, |entry| entry.factor);

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
    println!("Crypto rates updated successfully.");
    Ok(())
}
