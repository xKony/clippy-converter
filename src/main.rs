#![windows_subsystem = "windows"]

pub mod api;
pub mod autostart;
pub mod clipboard;
pub mod converter;
pub mod format;
pub mod db;
pub mod history;
pub mod hotkey;
pub mod models;
pub mod parser;
pub mod placement;
pub mod theme;
pub mod ui;
pub mod workers;

use anyhow::{Context, Result};
use db::Db;
use models::Config;
use single_instance::SingleInstance;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn main() -> Result<()> {
    init_tracing();

    // Ensure only one instance is running
    let instance = SingleInstance::new("com.clippy.clippy-converter")
        .context("Failed to create single instance lock")?;

    if !instance.is_single() {
        return Err(anyhow::anyhow!(
            "Another instance of Clippy Converter is already running. Exiting."
        ));
    }

    let config = Config::load().unwrap_or_else(|err| {
        error!(error = %err, "failed to load config; using defaults");
        Config::default()
    });

    let db = Db::open()
        .context("Failed to open database. Check if another process is using the database file.")?;

    if let Err(err) = db.init_static_units() {
        error!(error = %err, "failed to initialize static units");
    } else {
        info!("static units initialized");
    }

    ui::run(config, db)
}
