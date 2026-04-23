use anyhow::Result;
use std::time::Duration;

pub mod api;
pub mod models;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Clippy Converter starting...");

    // Spawn background currency refresher
    tokio::spawn(async move {
        loop {
            if let Err(e) = api::refresh_cache_if_needed().await {
                eprintln!("Background currency update failed: {e}");
            }
            // Check once per hour
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });

    println!("Background services initialized.");

    // TODO(#6): Initialize Global Hotkey Listener (Phase 6)
    // For now, keep the main task alive to verify the background loop
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}
