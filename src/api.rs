use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;
use tracing::warn;

/// `FawazAhmed` EUR-base feed, jsDelivr first with the documented Cloudflare Pages fallback.
const FIAT_API_URLS: &[&str] = &[
    "https://cdn.jsdelivr.net/npm/@fawazahmed0/currency-api@latest/v1/currencies/eur.json",
    "https://latest.currency-api.pages.dev/v1/currencies/eur.json",
];

/// Base URL for the Binance crypto price API.
const BINANCE_API_URL: &str = "https://api.binance.com/api/v3/ticker/price";

/// Quote currency suffix used to identify the crypto pairs we care about.
const USDT_SUFFIX: &str = "USDT";

/// Connection timeout applied to every request made with [`HTTP_CLIENT`].
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Overall request timeout applied to every request made with [`HTTP_CLIENT`].
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared `reqwest` client reused across all API calls.
///
/// Building a fresh client per-request (as `reqwest::get` does) discards
/// connection pooling and, more importantly, has no timeouts configured,
/// which can leave requests hanging indefinitely on a stalled connection.
/// Falls back to the default client (no explicit timeouts) if construction
/// somehow fails, so callers always get a usable client.
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_default()
});

/// Internal struct for parsing the `FawazAhmed` fiat API response.
#[derive(Debug, Deserialize)]
struct FawazAhmedResponse {
    /// The actual rates nested under the base currency key.
    pub eur: HashMap<String, f64>,
}

/// Internal struct for parsing a single Binance ticker price.
#[derive(Debug, Deserialize)]
pub(crate) struct BinanceTicker {
    /// The pair symbol (e.g., "BTCUSDT").
    pub symbol: String,
    /// The current price as a string.
    pub price: String,
}

/// Fetches the latest fiat currency rates from the `FawazAhmed` API.
///
/// Tries jsDelivr first, then the Cloudflare Pages fallback. HTTP error
/// statuses (`403`, `5xx`, …) fail that URL instead of being parsed as JSON.
///
/// # Errors
/// Returns an error if every URL fails to connect, returns a bad status, or cannot be parsed.
pub async fn fetch_fiat_rates() -> Result<HashMap<String, f64>> {
    let mut last_error = None;
    for url in FIAT_API_URLS {
        match fetch_fiat_from_url(url).await {
            Ok(rates) => return Ok(rates),
            Err(err) => {
                warn!(url, error = %err, "fiat currency API request failed");
                last_error = Some(err);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("no fiat API URLs configured")))
}

async fn fetch_fiat_from_url(url: &str) -> Result<HashMap<String, f64>> {
    let response = HTTP_CLIENT
        .get(url)
        .send()
        .await
        .context("Failed to connect to fiat currency API")?
        .error_for_status()
        .context("Fiat currency API returned an error status")?;
    let parsed: FawazAhmedResponse = response
        .json()
        .await
        .context("Failed to parse fiat currency API response")?;
    Ok(normalize_fiat_rates(parsed.eur))
}

fn normalize_fiat_rates(eur: HashMap<String, f64>) -> HashMap<String, f64> {
    let mut rates: HashMap<String, f64> = eur
        .into_iter()
        .map(|(key, value)| (key.to_uppercase(), value))
        .collect();
    rates.insert("EUR".to_string(), 1.0);
    rates
}

/// Fetches crypto price tickers from the Binance API, narrowed to `USDT`-quoted pairs.
///
/// The public Binance ticker/price endpoint returns every listed symbol
/// (spot, leveraged tokens, etc.), most of which we never use. Filtering
/// here avoids deserializing and shipping the full, much larger payload
/// through the rest of the pipeline.
///
/// # Errors
/// Returns an error if the network request fails or the response cannot be parsed.
pub(crate) async fn fetch_binance_tickers() -> Result<Vec<BinanceTicker>> {
    let response = HTTP_CLIENT
        .get(BINANCE_API_URL)
        .send()
        .await
        .context("Failed to connect to Binance API")?
        .error_for_status()
        .context("Binance API returned an error status")?;
    let tickers: Vec<BinanceTicker> = response
        .json()
        .await
        .context("Failed to parse Binance API response")?;

    Ok(filter_usdt_tickers(tickers))
}

/// Retains only tickers whose symbol ends with the `USDT` quote currency
/// (e.g. `BTCUSDT`), dropping every other quote/leveraged pair.
fn filter_usdt_tickers(tickers: Vec<BinanceTicker>) -> Vec<BinanceTicker> {
    tickers
        .into_iter()
        .filter(|ticker| ticker.symbol.ends_with(USDT_SUFFIX) && !is_noise_usdt_pair(&ticker.symbol))
        .collect()
}

/// Leveraged tokens and 1000x meme contracts that clutter the unit picker.
fn is_noise_usdt_pair(symbol: &str) -> bool {
    let Some(base) = symbol.strip_suffix(USDT_SUFFIX) else {
        return false;
    };
    base.starts_with("1000")
        || base.starts_with("1M")
        || base.ends_with("UP")
        || base.ends_with("DOWN")
        || base.ends_with("BULL")
        || base.ends_with("BEAR")
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
    fn normalize_fiat_rates_should_uppercase_symbols_and_insert_eur() {
        let mut eur = HashMap::new();
        eur.insert("usd".to_string(), 1.08);
        let rates = normalize_fiat_rates(eur);
        assert_eq!(rates.get("USD"), Some(&1.08));
        assert_eq!(rates.get("EUR"), Some(&1.0));
    }

    #[test]
    fn fiat_api_urls_should_include_cloudflare_pages_fallback() {
        assert!(FIAT_API_URLS[0].contains("jsdelivr.net"));
        assert!(FIAT_API_URLS[1].contains("currency-api.pages.dev"));
    }

    #[test]
    fn test_parse_binance_json() {
        let json = r#"[{"symbol":"BTCUSDT","price":"66341.21000000"},{"symbol":"ETHUSDT","price":"3211.55000000"}]"#;
        let tickers: Vec<BinanceTicker> = serde_json::from_str(json).unwrap();
        assert_eq!(tickers.len(), 2);
        assert_eq!(tickers[0].symbol, "BTCUSDT");
        assert_eq!(tickers[0].price, "66341.21000000");
    }

    fn ticker(symbol: &str) -> BinanceTicker {
        BinanceTicker {
            symbol: symbol.to_string(),
            price: "1.0".to_string(),
        }
    }

    #[test]
    fn test_filter_usdt_tickers_keeps_only_usdt_pairs() {
        let tickers = vec![
            ticker("BTCUSDT"),
            ticker("ETHBTC"),
            ticker("ETHUSDT"),
            ticker("BUSDUSDT"),
            ticker("EURUSDT"),
        ];

        let filtered = filter_usdt_tickers(tickers);
        let symbols: Vec<&str> = filtered.iter().map(|t| t.symbol.as_str()).collect();

        assert_eq!(symbols, vec!["BTCUSDT", "ETHUSDT", "BUSDUSDT", "EURUSDT"]);
    }

    #[test]
    fn test_filter_usdt_tickers_excludes_symbols_without_usdt_suffix() {
        let tickers = vec![ticker("USDTBTC"), ticker("ETHBUSD"), ticker("BNBBTC")];

        let filtered = filter_usdt_tickers(tickers);

        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_usdt_tickers_empty_input() {
        assert!(filter_usdt_tickers(Vec::new()).is_empty());
    }

    #[test]
    fn filter_usdt_tickers_should_drop_leveraged_and_thousand_tokens() {
        let tickers = vec![
            ticker("BTCUSDT"),
            ticker("1000PEPEUSDT"),
            ticker("BTCUPUSDT"),
            ticker("ETHDOWNUSDT"),
            ticker("ETHBULLUSDT"),
            ticker("1MBABYDOGEUSDT"),
            ticker("SOLUSDT"),
        ];
        let filtered = filter_usdt_tickers(tickers);
        let symbols: Vec<&str> = filtered.iter().map(|t| t.symbol.as_str()).collect();
        assert_eq!(symbols, vec!["BTCUSDT", "SOLUSDT"]);
    }
}
