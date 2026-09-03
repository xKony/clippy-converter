use crate::format::format_history_value;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use std::fs;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt, BufReader as AsyncBufReader};
use tracing::{debug, warn};

/// Marker file written after a successful prune; used to prune at most once per calendar day.
const PRUNE_MARKER_NAME: &str = ".history_pruned";

/// A parsed history log entry.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryItem {
    pub timestamp: Option<DateTime<Utc>>,
    pub input_value: f64,
    pub input_unit: String,
    pub output_value: f64,
    pub output_unit: String,
}

/// Byte size of one backward read step when scanning the history tail.
const TAIL_CHUNK_BYTES: u64 = 8 * 1024;

/// Returns the most recent history entries (newest first), up to `limit`.
///
/// The log is scanned backwards in chunks so a large file (possible when
/// retention is `Never`) costs a few KiB of I/O per popup open instead of a
/// full-file read.
///
/// # Errors
/// Returns an error if the history file cannot be opened or read.
pub fn list_recent(limit: usize) -> Result<Vec<HistoryItem>> {
    let path = get_history_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    list_recent_at(&path, limit)
}

/// Testable core of [`list_recent`], parameterized over an explicit path so
/// the bounded tail read can be exercised against a temp directory in tests.
fn list_recent_at(path: &Path, limit: usize) -> Result<Vec<HistoryItem>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut file = fs::File::open(path)
        .with_context(|| format!("Failed to open history log at {}", path.display()))?;
    let len = file
        .metadata()
        .with_context(|| format!("Failed to stat history log at {}", path.display()))?
        .len();

    let mut items: Vec<HistoryItem> = Vec::with_capacity(limit.min(64));
    // Bytes of the earliest line seen so far that may not be complete yet; it
    // is terminated either by the next (leftward) chunk or by the file start.
    let mut fragment: Vec<u8> = Vec::new();
    let mut end = len;

    while end > 0 && items.len() < limit {
        let start = end.saturating_sub(TAIL_CHUNK_BYTES);
        let chunk_len =
            usize::try_from(end - start).context("history log size exceeds platform limits")?;
        let mut buf = vec![0_u8; chunk_len];
        file.seek(SeekFrom::Start(start))
            .and_then(|_| file.read_exact(&mut buf))
            .with_context(|| format!("Failed to read history log at {}", path.display()))?;

        // Walk the window right-to-left, closing one line at each newline
        // byte. Splitting happens on raw bytes (`b'\n'` is ASCII) and each
        // finished line is decoded whole, so multi-byte UTF-8 units such as
        // `m²` are never corrupted by a chunk boundary.
        let mut cursor = buf.len();
        while items.len() < limit {
            let Some(nl) = buf[..cursor].iter().rposition(|&b| b == b'\n') else {
                // No newline left in this window: everything scanned so
                // far belongs to one still-open line.
                let mut joined = Vec::with_capacity(cursor + fragment.len());
                joined.extend_from_slice(&buf[..cursor]);
                joined.append(&mut fragment);
                fragment = joined;
                break;
            };
            let mut line = Vec::from(&buf[nl + 1..cursor]);
            line.extend_from_slice(&fragment);
            fragment.clear();
            if let Some(item) = parse_history_line(&String::from_utf8_lossy(&line)) {
                items.push(item);
            }
            cursor = nl;
        }
        end = start;
    }

    // The first line of the file has no preceding newline; it only terminates
    // once the scan reaches the very start.
    if items.len() < limit
        && let Some(item) = parse_history_line(&String::from_utf8_lossy(&fragment))
    {
        items.push(item);
    }

    Ok(items)
}

fn parse_history_line(line: &str) -> Option<HistoryItem> {
    // [timestamp] | 42.5000 kg -> 93.7000 lb
    let pipe_idx = line.find('|')?;
    let rest = line[pipe_idx + 1..].trim();
    let arrow_idx = rest.find("->")?;
    let left = rest[..arrow_idx].trim();
    let right = rest[arrow_idx + 2..].trim();

    let timestamp = line[1..pipe_idx]
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<DateTime<Utc>>()
        .ok();

    let (input_value, input_unit) = split_value_unit(left)?;
    let (output_value, output_unit) = split_value_unit(right)?;

    Some(HistoryItem {
        timestamp,
        input_value,
        input_unit,
        output_value,
        output_unit,
    })
}

/// Human-readable relative age of a history entry, bucketed by magnitude:
/// seconds collapse to "just now", then minutes, hours, and days.
#[must_use]
pub fn relative_age(timestamp: DateTime<Utc>) -> String {
    let elapsed = Utc::now() - timestamp;
    let secs = elapsed.num_seconds().max(0);
    if secs < 60 {
        "just now".to_string()
    } else if secs < 60 * 60 {
        format!("{}m ago", secs / 60)
    } else if secs < 60 * 60 * 24 {
        format!("{}h ago", secs / (60 * 60))
    } else {
        format!("{}d ago", secs / (60 * 60 * 24))
    }
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
        "[{timestamp}] | {} {input_unit} -> {} {output_unit}\n",
        format_history_value(input_value),
        format_history_value(output_value),
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
        let Some(end_idx) = line.find(']') else {
            // No timestamp delimiter at all: keep malformed lines just in case
            // instead of silently dropping them during the rewrite.
            kept_lines.push(line);
            continue;
        };
        if let Ok(ts) = DateTime::parse_from_rfc3339(&line[1..end_idx]) {
            if ts.with_timezone(&Utc) >= threshold {
                kept_lines.push(line);
            }
        } else {
            // Keep malformed lines just in case
            kept_lines.push(line);
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

/// Wipes the entire history log.
///
/// Uses the same crash-safe temp-file + rename pattern as
/// [`prune_history_atomic`] so an interrupted clear can never leave a
/// half-written log behind. A missing history file is treated as already clear.
///
/// # Errors
/// Returns an error if creating the empty replacement file or renaming it
/// over the original fails.
pub async fn clear_history() -> Result<()> {
    let path = get_history_path()?;
    clear_history_at(&path).await
}

/// Testable core of [`clear_history`], parameterized over an explicit path so
/// the wipe behavior can be exercised against a temp directory in tests.
async fn clear_history_at(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = parent.join(format!(
        "history.{}.tmp",
        Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));

    tokio::fs::File::create(&temp_path)
        .await
        .with_context(|| format!("Failed to create temp history at {}", temp_path.display()))?;

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
    use std::fmt::Write as _;

    /// Relative-tolerance float assertion for test expectations.
    fn assert_relative_eq(a: f64, b: f64) {
        let tolerance = 1e-9 * a.abs().max(b.abs()).max(1.0);
        assert!(
            (a - b).abs() <= tolerance,
            "expected {a} to equal {b} within relative tolerance"
        );
    }

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
        assert_relative_eq(item.input_value, 42.5);
        assert_eq!(item.input_unit, "kg");
        assert_relative_eq(item.output_value, 93.7);
        assert_eq!(item.output_unit, "lb");
    }

    #[test]
    fn parse_history_line_should_keep_timestamp_when_present() {
        let line = "[2024-04-23T10:00:00Z] | 42.5000 kg -> 93.7000 lb";
        let item = parse_history_line(line).unwrap();
        let expected = DateTime::parse_from_rfc3339("2024-04-23T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(item.timestamp, Some(expected));
    }

    #[test]
    fn parse_history_line_should_drop_unparseable_timestamp() {
        let line = "[not-a-date] | 9.9999 x -> 9.9999 y";
        let item = parse_history_line(line).unwrap();
        assert_eq!(item.timestamp, None);
    }

    #[test]
    fn relative_age_should_report_just_now_within_a_minute() {
        let ts = Utc::now() - chrono::Duration::seconds(30);
        assert_eq!(relative_age(ts), "just now");
    }

    #[test]
    fn relative_age_should_report_minutes_under_an_hour() {
        let ts = Utc::now() - chrono::Duration::minutes(5);
        assert_eq!(relative_age(ts), "5m ago");
    }

    #[test]
    fn relative_age_should_report_hours_under_a_day() {
        let ts = Utc::now() - chrono::Duration::hours(2);
        assert_eq!(relative_age(ts), "2h ago");
    }

    #[test]
    fn relative_age_should_report_days_beyond_a_day() {
        let ts = Utc::now() - chrono::Duration::days(3);
        assert_eq!(relative_age(ts), "3d ago");
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

        let items = list_recent_at(&path, 10).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].input_unit, "kg");
        assert_eq!(items[1].input_unit, "m");
    }

    #[test]
    fn list_recent_at_should_read_only_the_tail_of_a_large_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        let mut contents = String::new();
        for i in 0..1000 {
            let _ = writeln!(
                contents,
                "[2024-04-23T10:{i:02}:00Z] | {i}.0000 kg -> {i}.0000 lb"
            );
        }
        std::fs::write(&path, contents).unwrap();

        let items = list_recent_at(&path, 10).unwrap();

        assert_eq!(items.len(), 10);
        assert_eq!(items[0].input_value, 999.0);
        assert_eq!(items[9].input_value, 990.0);
    }

    #[test]
    fn list_recent_at_should_keep_scanning_past_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        std::fs::write(
            &path,
            "[t1] | 1.0000 m -> 3.2808 ft\ngarbage\n[t2] | 2.0000 kg -> 4.4092 lb\n",
        )
        .unwrap();

        let items = list_recent_at(&path, 2).unwrap();

        // Both valid entries are returned newest-first despite the garbage
        // line between them.
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].input_unit, "kg");
        assert_eq!(items[1].input_unit, "m");
    }

    #[test]
    fn list_recent_at_should_handle_missing_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        std::fs::write(
            &path,
            "[t1] | 1.0000 m -> 3.2808 ft\n[t2] | 2.0000 kg -> 4.4092 lb",
        )
        .unwrap();

        let items = list_recent_at(&path, 10).unwrap();

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].input_unit, "kg");
        assert_eq!(items[1].input_unit, "m");
    }

    #[test]
    fn list_recent_at_should_survive_lines_spanning_chunk_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        let long_unit = "x".repeat(usize::try_from(TAIL_CHUNK_BYTES).unwrap() * 3);
        let line = format!("[t1] | 1.0000 {long_unit} -> 2.0000 y");
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let items = list_recent_at(&path, 5).unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].input_unit, long_unit);
    }

    #[test]
    fn list_recent_at_should_decode_utf8_units() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        // A log wider than one read chunk whose entry carries a multi-byte
        // unit: decoding must happen per completed line, never mid-chunk.
        let pad_len = usize::try_from(TAIL_CHUNK_BYTES).unwrap();
        let padding = "p".repeat(pad_len);
        std::fs::write(&path, format!("[t1] | 1.0000 {padding} -> 2.0000 m²\n")).unwrap();

        let items = list_recent_at(&path, 5).unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].output_unit, "m²");
    }

    #[test]
    fn list_recent_at_should_return_empty_for_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        std::fs::write(&path, "").unwrap();

        let items = list_recent_at(&path, 10).unwrap();

        assert!(items.is_empty());
    }

    #[test]
    fn list_recent_at_should_cap_entries_when_limit_is_smaller_than_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        std::fs::write(
            &path,
            "[t1] | 1.0000 m -> 3.2808 ft\n[t2] | 2.0000 kg -> 4.4092 lb\n[t3] | 3.0000 s -> 3000 ms\n",
        )
        .unwrap();

        let items = list_recent_at(&path, 2).unwrap();

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].input_value, 3.0);
        assert_eq!(items[1].input_value, 2.0);
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
    async fn prune_history_atomic_keeps_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        let old = "2020-01-01T00:00:00Z";
        let recent = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        std::fs::write(
            &path,
            format!(
                "[{old}] | 1.0000 m -> 3.2808 ft\nno closing bracket at all\n[not-a-date] | 9.9999 x -> 9.9999 y\n[{recent}] | 2.0000 kg -> 4.4092 lb\n"
            ),
        )
        .unwrap();

        prune_history_atomic(&path, 30).await.unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        // Only the entry with an old parsed timestamp may be dropped.
        assert!(!contents.contains("1.0000 m"));
        assert!(contents.contains("2.0000 kg"));
        // Malformed lines (missing `]`, unparseable timestamp) must survive.
        assert!(contents.contains("no closing bracket at all"));
        assert!(contents.contains("[not-a-date] | 9.9999 x -> 9.9999 y"));
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
    async fn clear_history_at_should_empty_the_log_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");
        std::fs::write(
            &path,
            "[2024-04-23T10:00:00Z] | 1.0000 m -> 3.2808 ft\n[2024-04-24T10:00:00Z] | 2.0000 kg -> 4.4092 lb\n",
        )
        .unwrap();

        clear_history_at(&path).await.unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.is_empty());

        // The atomic wipe must not leave any leftover temp files behind.
        let mut read_dir = tokio::fs::read_dir(dir.path()).await.unwrap();
        while let Some(entry) = read_dir.next_entry().await.unwrap() {
            assert!(!entry.file_name().to_string_lossy().ends_with(".tmp"));
        }
    }

    #[tokio::test]
    async fn clear_history_at_should_succeed_when_log_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.log");

        clear_history_at(&path).await.unwrap();

        assert!(!path.exists());
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
