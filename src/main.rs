use crate::clipboard::ClipboardManager;
use crate::converter::Converter;
use crate::models::{Cache, Config};
use anyhow::{Context, Result};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use std::time::Duration;

pub mod api;
pub mod clipboard;
pub mod converter;
pub mod hotkey;
pub mod models;
pub mod parser;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Clippy Converter starting...");

    // 1. Load configuration and cache
    let config = Config::load().context("Failed to load config")?;
    let cache = Cache::load().context("Failed to load cache")?;

    // 2. Initialize core components
    let converter = Converter::new(config.clone(), cache.clone());
    let mut clipboard =
        ClipboardManager::new().context("Failed to initialize clipboard manager")?;
    let manager = GlobalHotKeyManager::new().context("Failed to initialize hotkey manager")?;

    // 3. Register hotkey
    let hk = hotkey::parse_hotkey(&config.hotkey).context("Failed to parse hotkey")?;
    manager
        .register(hk)
        .context("Failed to register global hotkey")?;

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
    println!("Listening for hotkey: {}...", config.hotkey);

    let receiver = GlobalHotKeyEvent::receiver();

    // Main event loop
    loop {
        if let Ok(event) = receiver.try_recv()
            && event.id == hk.id()
        {
            println!("Hotkey triggered!");

            match clipboard.capture_selection() {
                Ok(text) => {
                    println!("Captured: '{text}'");
                    match parser::parse_input(&text) {
                        Ok(parsed) => {
                            match converter
                                .convert(parsed.value, parsed.unit.as_deref().unwrap_or(""))
                            {
                                Ok(result) => {
                                    println!(
                                        "Conversion results for {} {}:",
                                        result.input_value, result.input_unit
                                    );
                                    for output in result.outputs {
                                        println!("  {:.2} {}", output.value, output.unit);
                                    }
                                }
                                Err(e) => eprintln!("Conversion failed: {e}"),
                            }
                        }
                        Err(e) => eprintln!("Parsing failed: {e}"),
                    }
                }
                Err(e) => eprintln!("Capture failed: {e}"),
            }
        }

        // Small sleep to prevent high CPU usage while polling
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
