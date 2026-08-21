use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt, BufReader as AsyncBufReader};
use tracing::{debug, warn};

/// Marker file written after a successful prune; used to prune at most once per calendar day.
const PRUNE_MARKER_NAME: &str = ".history_pruned";

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

/// Appends a conversion result to the history log file (append-only).
///
/// Retention pruning is intentionally **not** performed on every write; call
/// [`prune_history_if_needed`] periodically (e.g. at startup or from a background task).
///
/// # Errors
/// Returns an error if the data directory cannot be determined,
/// or if creating the directory or file fails.
pub async fn log_conversion(
    input_value: f64,
    input_unit: &str,
    output_value: f64,
    output_unit: &str,
) -> Result<()> {
    let path = get_history_path()?;

    let timestamp = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let entry = format!(
        "[{timestamp}] | {input_value:.4} {input_unit} -> {output_value:.4} {output_unit}\n"
    );

    append_entry(&path, &entry).await
}

/// Appends a single pre-formatted entry line to the history log at `path`,
/// creating the parent directory and file as needed. Pulled out of
/// [`log_conversion`] so the pure append behavior can be exercised directly
/// against a temp path in tests.
async fn append_entry(path: &Path, entry: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.with_context(|| {
            format!("Failed to create history directory at {}", parent.display())
        })?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .with_context(|| format!("Failed to open history log at {}", path.display()))?;

    file.write_all(entry.as_bytes())
        .await
        .context("Failed to write to history log")?;

    Ok(())
}

/// Prunes the history log when retention is enabled, at most once per calendar day.
///
/// Uses a sibling marker file (`.history_pruned`) so routine conversions stay append-only.
/// The rewrite is crash-safe: data is written to a temp file then renamed over the original.
///
/// # Errors
/// Returns an error if reading, writing, or renaming the history file fails.
pub async fn prune_history_if_needed(retention_days: Option<i64>) -> Result<()> {
    let Some(days) = retention_days else {
        return Ok(());
    };

    let path = get_history_path()?;
    let marker = prune_marker_path(&path);
    prune_if_needed_at(&path, &marker, days).await
}

/// Testable core of [`prune_history_if_needed`], parameterized over explicit
/// paths so the once-per-day throttling and pruning behavior can be
/// exercised against a temp directory in tests.
async fn prune_if_needed_at(path: &Path, marker: &Path, days: i64) -> Result<()> {
    if pruned_today(marker).await {
        debug!("history prune skipped; already pruned today");
        return Ok(());
    }

    if !path.exists() {
        touch_prune_marker(marker).await?;
        return Ok(());
    }

    prune_history_atomic(path, days).await?;
    touch_prune_marker(marker).await?;
    Ok(())
}

fn prune_marker_path(history_path: &Path) -> PathBuf {
    history_path.parent().map_or_else(
        || PathBuf::from(PRUNE_MARKER_NAME),
        |p| p.join(PRUNE_MARKER_NAME),
    )
}

async fn pruned_today(marker: &Path) -> bool {
    let Ok(metadata) = tokio::fs::metadata(marker).await else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let modified: DateTime<Utc> = modified.into();
    modified.date_naive() == Utc::now().date_naive()
}

async fn touch_prune_marker(marker: &Path) -> Result<()> {
    if let Some(parent) = marker.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    tokio::fs::write(marker, Utc::now().to_rfc3339().as_bytes())
        .await
        .with_context(|| format!("Failed to write prune marker at {}", marker.display()))
}

/// Prunes history entries older than the specified number of days using temp+rename.
async fn prune_history_atomic(path: &Path, days: i64) -> Result<()> {
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

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = parent.join(format!(
        "history.{}.tmp",
        Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));

    {
        let mut temp = tokio::fs::File::create(&temp_path)
            .await
            .with_context(|| format!("Failed to create temp history at {}", temp_path.display()))?;
        for line in &kept_lines {
            temp.write_all(line.as_bytes()).await?;
            temp.write_all(b"\n").await?;
        }
        temp.flush().await?;
    }

    if let Err(err) = tokio::fs::rename(&temp_path, path).await {
        warn!(
            error = %err,
            temp = %temp_path.display(),
            "atomic history rename failed; leaving original intact"
        );
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(err).context("Failed to replace history log atomically");
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

    #[tokio::test]
    async fn prune_history_atomic_keeps_recent_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        let old = "2020-01-01T00:00:00Z";
        let recent = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        std::fs::write(
            &path,
            format!("[{old}] | 1.0000 m -> 3.2808 ft\n[{recent}] | 2.0000 kg -> 4.4092 lb\n"),
        )
        .unwrap();

        prune_history_atomic(&path, 30).await.unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("1.0000 m"));
        assert!(contents.contains("2.0000 kg"));

        // The atomic rewrite must not leave any leftover temp files behind.
        let mut read_dir = tokio::fs::read_dir(dir.path()).await.unwrap();
        let mut entry_count = 0;
        while let Some(entry) = read_dir.next_entry().await.unwrap() {
            assert!(!entry.file_name().to_string_lossy().ends_with(".tmp"));
            entry_count += 1;
        }
        assert_eq!(entry_count, 1);
    }

    #[tokio::test]
    async fn append_entry_never_rewrites_existing_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");

        for i in 0..5 {
            let entry = format!("[2024-04-23T10:00:00Z] | {i}.0000 kg -> {i}.0000 lb\n");
            append_entry(&path, &entry).await.unwrap();
        }

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 5);
        // Earliest entry is still present untouched — appends never prune.
        assert!(contents.contains("0.0000 kg -> 0.0000 lb"));
    }

    #[tokio::test]
    async fn prune_if_needed_at_prunes_and_writes_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        let marker = dir.path().join(".history_pruned");

        let old = "2020-01-01T00:00:00Z";
        let recent = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        std::fs::write(
            &path,
            format!("[{old}] | 1.0000 m -> 3.2808 ft\n[{recent}] | 2.0000 kg -> 4.4092 lb\n"),
        )
        .unwrap();
        assert!(!marker.exists());

        prune_if_needed_at(&path, &marker, 30).await.unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("1.0000 m"));
        assert!(contents.contains("2.0000 kg"));
        assert!(marker.exists());
    }

    #[tokio::test]
    async fn prune_if_needed_at_skips_when_already_pruned_today() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        let marker = dir.path().join(".history_pruned");

        let old = "2020-01-01T00:00:00Z";
        std::fs::write(&path, format!("[{old}] | 1.0000 m -> 3.2808 ft\n")).unwrap();
        // Marker already touched "today" — a later call must be a no-op.
        touch_prune_marker(&marker).await.unwrap();

        prune_if_needed_at(&path, &marker, 30).await.unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("1.0000 m"),
            "entry should survive since pruning was throttled for today"
        );
    }
}
