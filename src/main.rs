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

use anyhow::Result;
use db::Db;
use models::Config;

fn main() -> Result<()> {
    println!("Clippy Converter starting...");

    let config = Config::load().unwrap_or_default();
    let db = Db::open()?;
    
    iced::daemon(move || ui::boot(ui::BootParams { config: config.clone(), db: db.clone() }), ui::update, ui::view)
        .subscription(ui::subscription)
        .run()?;

    Ok(())
}
