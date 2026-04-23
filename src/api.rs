use crate::models::Cache;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserialize;
use std::collections::HashMap;

/// Base URL for the Frankfurter currency API.
const API_BASE_URL: &str = "https://api.frankfurter.app/latest";

/// Internal struct for parsing the Frankfurter API response.
#[derive(Debug, Deserialize)]
struct FrankfurterResponse {
    /// The base currency (usually "EUR").
    pub base: String,
    /// Mapping of target currencies to their rates relative to the base.
    pub rates: HashMap<String, f64>,
}

/// Fetches the latest currency rates from the Frankfurter API.
///
/// This function retrieves rates relative to the default base (EUR)
/// and includes the base currency with a rate of 1.0.
///
/// # Errors
/// Returns an error if the network request fails or the response cannot be parsed.
pub async fn fetch_latest_rates() -> Result<HashMap<String, f64>> {
    let response: FrankfurterResponse = reqwest::get(API_BASE_URL)
        .await
        .context("Failed to connect to currency API")?
        .json()
        .await
        .context("Failed to parse currency API response")?;

    let mut rates = response.rates;
    // Always include the base rate
    rates.insert(response.base, 1.0);

    Ok(rates)
}

/// Checks the local cache and updates it from the API if it is expired or empty.
///
/// # Errors
/// Returns an error if the cache cannot be loaded/saved or if the network fetch fails.
pub async fn refresh_cache_if_needed() -> Result<()> {
    let mut cache = Cache::load().context("Failed to load currency cache")?;

    if cache.is_expired() {
        println!("Refreshing currency rates...");
        let rates = fetch_latest_rates().await?;
        cache.rates = rates;
        cache.last_updated = Utc::now();
        cache
            .save()
            .context("Failed to save updated currency cache")?;
        println!("Currency rates updated successfully.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frankfurter_json() {
        let json = r#"{
            "amount": 1.0,
            "base": "EUR",
            "date": "2024-04-23",
            "rates": {
                "USD": 1.0658,
                "PLN": 4.3123
            }
        }"#;
        let response: FrankfurterResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.base, "EUR");
        assert_eq!(response.rates.get("USD"), Some(&1.0658));
    }
}
