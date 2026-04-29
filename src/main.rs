#![windows_subsystem = "windows"]

pub mod api;
pub mod clipboard;
pub mod converter;
pub mod db;
pub mod history;
pub mod hotkey;
pub mod models;
pub mod parser;
pub mod theme;
pub mod ui;
pub mod workers;

use anyhow::{Context, Result};
use db::Db;
use models::Config;
use single_instance::SingleInstance;

fn main() -> Result<()> {
    // Ensure only one instance is running
    let instance = SingleInstance::new("com.clippy.clippy-converter")
        .context("Failed to create single instance lock")?;

    if !instance.is_single() {
        return Err(anyhow::anyhow!(
            "Another instance of Clippy Converter is already running. Exiting."
        ));
    }

    let config = Config::load().unwrap_or_default();

    let db = Db::open()
        .context("Failed to open database. Check if another process is using the database file.")?;

    if let Err(e) = db.init_static_units() {
        // We can keep this as a silent error or use a log, but avoiding eprintln for now
        let _ = e;
    }

    ui::run(config, db)
}
