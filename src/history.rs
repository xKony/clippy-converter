use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use std::path::PathBuf;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Appends a conversion result to the history log file and prunes old entries.
///
/// This is intended to be called within `tokio::spawn` to avoid blocking.
///
/// # Errors
/// Returns an error if the data directory cannot be determined,
/// or if creating the directory or file fails.
pub async fn log_conversion(
    input_value: f64,
    input_unit: &str,
    output_value: f64,
    output_unit: &str,
    retention_days: Option<i64>,
) -> Result<()> {
    let path = get_history_path()?;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.with_context(|| {
            format!("Failed to create history directory at {}", parent.display())
        })?;
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

    file.write_all(entry.as_bytes())
        .await
        .context("Failed to write to history log")?;

    if let Some(days) = retention_days {
        let _ = prune_history(&path, days).await;
    }

    Ok(())
}

/// Prunes history entries older than the specified number of days.
async fn prune_history(path: &std::path::Path, days: i64) -> Result<()> {
    let Ok(file) = tokio::fs::File::open(path).await else {
        return Ok(());
    };

    let now = Utc::now();
    let threshold = now - chrono::Duration::days(days);
    let mut kept_lines = Vec::new();
    let mut reader = BufReader::new(file).lines();

    while let Some(line) = reader.next_line().await? {
        // Entry format: [2024-04-23T10:00:00Z] | ...
        if let Some(end_idx) = line.find(']') {
            if let Ok(ts) = DateTime::parse_from_rfc3339(&line[1..end_idx]) {
                if ts.with_timezone(&Utc) >= threshold {
                    kept_lines.push(line);
                }
            } else {
                // Keep malformed lines just in case
                kept_lines.push(line);
            }
        }
    }

    let mut file = tokio::fs::File::create(path).await?;
    for line in kept_lines {
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
    }

    Ok(())
}

/// Helper to get the path to the history log file.
///
/// # Errors
/// Returns an error if the application data directory cannot be determined.
pub fn get_history_path() -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "clippy", "clippy-converter")
        .context("Could not determine application data directory")?;
    Ok(proj_dirs.data_dir().join("history.log"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;

    #[tokio::test]
    async fn test_log_conversion_path() {
        let path = get_history_path();
        assert!(path.is_ok());
        assert!(path.unwrap().ends_with("history.log"));
    }
}
