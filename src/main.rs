pub mod api;
pub mod clipboard;
pub mod converter;
pub mod db;
pub mod history;
pub mod hotkey;
pub mod models;
pub mod parser;
pub mod ui;
pub mod workers;

use anyhow::{Context, Result};
use db::Db;
use models::Config;
use single_instance::SingleInstance;
use std::process;

fn main() -> Result<()> {
    // Ensure only one instance is running
    let instance = SingleInstance::new("com.clippy.clippy-converter")
        .context("Failed to create single instance lock")?;

    if !instance.is_single() {
        eprintln!("Another instance of Clippy Converter is already running. Exiting.");
        process::exit(1);
    }

    println!("Clippy Converter starting...");

    let config = Config::load().unwrap_or_default();
    
    let db = match Db::open() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Failed to open database: {e}");
            eprintln!("Check if another process is using the database file.");
            process::exit(1);
        }
    };
    
    iced::daemon(move || ui::boot(ui::BootParams { config: config.clone(), db: db.clone() }), ui::update, ui::view)
        .subscription(ui::subscription)
        .run()?;

    Ok(())
}
