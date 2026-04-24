use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

/// Base URL for the `FawazAhmed` fiat currency API (EUR base).
const FIAT_API_URL: &str =
    "https://cdn.jsdelivr.net/npm/@fawazahmed0/currency-api@latest/v1/currencies/eur.json";

/// Base URL for the Binance crypto price API.
const BINANCE_API_URL: &str = "https://api.binance.com/api/v3/ticker/price";

/// Internal struct for parsing the `FawazAhmed` fiat API response.
#[derive(Debug, Deserialize)]
struct FawazAhmedResponse {
    /// The actual rates nested under the base currency key.
    pub eur: HashMap<String, f64>,
}

/// Internal struct for parsing a single Binance ticker price.
#[derive(Debug, Deserialize)]
pub struct BinanceTicker {
    /// The pair symbol (e.g., "BTCUSDT").
    pub symbol: String,
    /// The current price as a string.
    pub price: String,
}

/// Fetches the latest fiat currency rates from the `FawazAhmed` API.
///
/// # Errors
/// Returns an error if the network request fails or the response cannot be parsed.
pub async fn fetch_fiat_rates() -> Result<HashMap<String, f64>> {
    let response: FawazAhmedResponse = reqwest::get(FIAT_API_URL)
        .await
        .context("Failed to connect to fiat currency API")?
        .json()
        .await
        .context("Failed to parse fiat currency API response")?;

    let mut rates = response.eur;
    // Ensure symbols are uppercase for consistency
    rates = rates
        .into_iter()
        .map(|(k, v)| (k.to_uppercase(), v))
        .collect();

    // Always include the base rate
    rates.insert("EUR".to_string(), 1.0);

    Ok(rates)
}

/// Fetches all crypto price tickers from the Binance API.
///
/// # Errors
/// Returns an error if the network request fails or the response cannot be parsed.
pub async fn fetch_binance_tickers() -> Result<Vec<BinanceTicker>> {
    let tickers: Vec<BinanceTicker> = reqwest::get(BINANCE_API_URL)
        .await
        .context("Failed to connect to Binance API")?
        .json()
        .await
        .context("Failed to parse Binance API response")?;

    Ok(tickers)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;

    #[test]
    fn test_parse_fawazahmed_json() {
        let json = r#"{
            "date": "2024-04-23",
            "eur": {
                "usd": 1.0658,
                "pln": 4.3123
            }
        }"#;
        let response: FawazAhmedResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.eur.get("usd"), Some(&1.0658));
    }

    #[test]
    fn test_parse_binance_json() {
        let json = r#"[{"symbol":"BTCUSDT","price":"66341.21000000"},{"symbol":"ETHUSDT","price":"3211.55000000"}]"#;
        let tickers: Vec<BinanceTicker> = serde_json::from_str(json).unwrap();
        assert_eq!(tickers.len(), 2);
        assert_eq!(tickers[0].symbol, "BTCUSDT");
        assert_eq!(tickers[0].price, "66341.21000000");
    }
}
