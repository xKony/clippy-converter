pub mod api;
pub mod clipboard;
pub mod converter;
pub mod history;
pub mod hotkey;
pub mod models;
pub mod parser;
pub mod ui;

use anyhow::Result;

fn main() -> Result<()> {
    println!("Clippy Converter starting...");

    iced::daemon("Clippy Converter", ui::update, ui::view)
        .subscription(ui::subscription)
        .run_with(ui::boot)?;

    Ok(())
}
