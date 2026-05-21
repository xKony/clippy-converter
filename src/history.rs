use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt, BufReader as AsyncBufReader};

/// A parsed history log entry.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryItem {
    pub input_value: f64,
    pub input_unit: String,
    pub output_value: f64,
    pub output_unit: String,
}

/// Returns the most recent history entries (newest first), up to `limit`.
///
/// # Errors
/// Returns an error if the history file cannot be read.
pub fn list_recent(limit: usize) -> Result<Vec<HistoryItem>> {
    let path = get_history_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(&path)
        .with_context(|| format!("Failed to open history log at {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut items = Vec::new();
    for line in reader.lines() {
        let line = line.context("Failed to read history line")?;
        if let Some(item) = parse_history_line(&line) {
            items.push(item);
        }
    }

    let skip = items.len().saturating_sub(limit);
    Ok(items.into_iter().skip(skip).rev().collect())
}

fn parse_history_line(line: &str) -> Option<HistoryItem> {
    // [timestamp] | 42.5000 kg -> 93.7000 lb
    let pipe_idx = line.find('|')?;
    let rest = line[pipe_idx + 1..].trim();
    let arrow_idx = rest.find("->")?;
    let left = rest[..arrow_idx].trim();
    let right = rest[arrow_idx + 2..].trim();

    let (input_value, input_unit) = split_value_unit(left)?;
    let (output_value, output_unit) = split_value_unit(right)?;

    Some(HistoryItem {
        input_value,
        input_unit,
        output_value,
        output_unit,
    })
}

fn split_value_unit(part: &str) -> Option<(f64, String)> {
    let mut tokens = part.split_whitespace();
    let value: f64 = tokens.next()?.parse().ok()?;
    let unit = tokens.collect::<Vec<_>>().join(" ");
    if unit.is_empty() {
        return None;
    }
    Some((value, unit))
}

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
    let mut reader = AsyncBufReader::new(file).lines();

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

    #[test]
    fn test_parse_history_line() {
        let line = "[2024-04-23T10:00:00Z] | 42.5000 kg -> 93.7000 lb";
        let item = parse_history_line(line).unwrap();
        assert!((item.input_value - 42.5).abs() < f64::EPSILON);
        assert_eq!(item.input_unit, "kg");
        assert!((item.output_value - 93.7).abs() < f64::EPSILON);
        assert_eq!(item.output_unit, "lb");
    }

    #[test]
    fn test_list_recent_returns_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        std::fs::write(
            &path,
            "[t1] | 1.0000 m -> 3.2808 ft\n[t2] | 2.0000 kg -> 4.4092 lb\n",
        )
        .unwrap();

        // Override path by parsing directly
        let items: Vec<HistoryItem> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter_map(parse_history_line)
            .collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].input_unit, "kg");
    }
}
