use anyhow::{Context, Result};
use chrono::Utc;
use directories::ProjectDirs;
use std::path::PathBuf;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

/// Appends a conversion result to the history log file.
/// 
/// This is intended to be called within `tokio::spawn` to avoid blocking.
///
/// # Errors
/// Returns an error if the data directory cannot be determined,
/// or if creating the directory or file fails.
pub async fn log_conversion(input_value: f64, input_unit: &str, output_value: f64, output_unit: &str) -> Result<()> {
    let path = get_history_path()?;
    
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await
            .with_context(|| format!("Failed to create history directory at {}", parent.display()))?;
    }

    let timestamp = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let entry = format!(
        "[{timestamp}] | {input_value:.4} {input_unit} -> {output_value:.4} {output_unit}\n"
    );

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .with_context(|| format!("Failed to open history log at {}", path.display()))?;

    file.write_all(entry.as_bytes()).await
        .context("Failed to write to history log")?;

    Ok(())
}

/// Helper to get the path to the history log file.
///
/// # Errors
/// Returns an error if the application data directory cannot be determined.
pub fn get_history_path() -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "konyy", "clippy-converter")
        .context("Could not determine application data directory")?;
    Ok(proj_dirs.data_dir().join("history.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_log_conversion_path() {
        let path = get_history_path();
        assert!(path.is_ok());
        assert!(path.unwrap().ends_with("history.log"));
    }
}
