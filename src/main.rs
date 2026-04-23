use anyhow::Result;

pub mod models;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Clippy Converter starting...");
    Ok(())
}
