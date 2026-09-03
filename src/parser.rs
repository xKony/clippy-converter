use anyhow::{Result, anyhow};
use chrono::{DateTime, Duration, FixedOffset, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

const CURRENCY_SYMBOLS: [(char, &str); 8] = [
    ('$', "USD"),
    ('€', "EUR"),
    ('£', "GBP"),
    ('¥', "JPY"),
    ('₹', "INR"),
    ('₪', "ILS"),
    ('₩', "KRW"),
    ('₽', "RUB"),
];

/// How a wall-clock input located itself in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedZone {
    /// No designator given: the OS-local zone.
    Local,
    /// Explicit `UTC`, `GMT`, or a trailing `Z`.
    Utc,
    /// A fixed offset such as `+05:30`.
    Fixed(FixedOffset),
    /// An IANA zone id or mapped abbreviation (`Europe/Warsaw`, `PST`).
    Tz(Tz),
}

/// A detected absolute moment plus the zone its source text referenced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallClock {
    /// Seconds since the Unix epoch (negative before 1970).
    pub epoch_seconds: i64,
    /// The zone the wall-clock text was expressed in.
    pub source_zone: ResolvedZone,
}

/// Result of a successful string parse.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedInput {
    /// The numeric value extracted from the string.
    pub value: f64,
    /// Upper endpoint when the input was a range (`10-20 USD`), otherwise `None`.
    pub end_value: Option<f64>,
    /// The optional source unit symbol or abbreviation extracted.
    pub unit: Option<String>,
    /// Optional explicit target from phrases like `100 USD to PLN`.
    pub target: Option<String>,
    /// Wall-clock payload (`17:30 UTC`, ISO datetimes, epochs tagged `utc`).
    ///
    /// Never coexists with [`ParsedInput::end_value`]: ranges are purely
    /// numeric shapes, datetime detection runs before range splitting.
    pub wall_clock: Option<WallClock>,
}

/// Parses a string into a numeric value and an optional unit.
///
/// Supports leading/trailing currency symbols, grouped digits (`1,234.56` /
/// `1.234,56`), `to`/`in` target clauses, and wall-clock inputs (`17:30 UTC`,
/// RFC 3339 strings, integers tagged `utc`/`unix`/`epoch`) which carry a
/// [`WallClock`] payload for the converter's timezone branch.
///
/// # Errors
/// Returns an error if no number can be found in the input string.
pub fn parse_input(input: &str) -> Result<ParsedInput> {
    let input = input.trim();
    if input.is_empty() {
        return Err(anyhow!("Empty input string"));
    }

    if let Some(detected) = detect_wall_clock(input) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "epoch seconds stay far below f64's exact-integer range"
        )]
        let value = detected.moment.epoch_seconds as f64;
        return Ok(ParsedInput {
            value,
            end_value: None,
            unit: Some(input.to_string()),
            target: detected.target.map(str::to_string),
            wall_clock: Some(detected.moment),
        });
    }

    let mut symbol_unit = None;
    let mut core_input = input;

    for (sym, unit) in CURRENCY_SYMBOLS {
        if input.starts_with(sym) {
            symbol_unit = Some(unit);
            core_input = input[sym.len_utf8()..].trim();
            break;
        }
    }

    // Ranges must be detected before the single-value scan runs: otherwise the
    // separator is misread as the start of the unit (`10-20 USD` -> `-20 USD`).
    if let Some((start, end, unit_text)) = split_range(core_input) {
        let (unit, target) = finalize_unit_and_target(unit_text, symbol_unit);
        return Ok(ParsedInput {
            value: start,
            end_value: Some(end),
            unit,
            target,
            wall_clock: None,
        });
    }

    let (number_end, found_digit) = scan_number_end(core_input);
    if !found_digit {
        return Err(anyhow!("No numeric value found in: {input}"));
    }

    let value_raw = &core_input[..number_end];
    let value_str = normalize_numeric(value_raw).ok_or_else(|| {
        anyhow!("Ambiguous numeric token with both a decimal comma and an exponent: {value_raw}")
    })?;
    let value: f64 = value_str
        .parse()
        .map_err(|_| anyhow!("Failed to parse numeric part: {value_raw}"))?;

    let (unit, target) = finalize_unit_and_target(&core_input[number_end..], symbol_unit);

    Ok(ParsedInput {
        value,
        end_value: None,
        unit,
        target,
        wall_clock: None,
    })
}

/// A wall-clock detection hit: the resolved moment plus an optional explicit
/// target zone from a trailing `to <zone>` clause.
struct DetectedTime<'a> {
    moment: WallClock,
    target: Option<&'a str>,
}

/// Detects wall-clock inputs before the numeric fallback paths run.
///
/// Accepted shapes (whole trimmed input, case-insensitive keywords):
/// - `1755792000 utc` / `123 unix` / `42 epoch` (bare integer + tag),
/// - RFC 3339 / ISO 8601 date-times: `2026-08-21T14:00Z`,
///   `2026-08-21T14:00:00+02:00`, and the spaced form `2026-08-21 14:00 UTC`
///   (fractional seconds are accepted and truncated),
/// - time fragments with optional zone designator: `17:30`, `17:30 UTC`,
///   `17:30Z`, `09:15+05:30`, `17:30 Europe/Warsaw` (date-less fragments use
///   today's date in their zone; untagged ones count as OS-local),
/// - each shape may end in a target clause (` to CET`, ` in Tokyo`).
///
/// Anything that matches the *shape* but fails resolution (unknown zone name)
/// or has out-of-range components (`25:99`) returns `None` so the caller falls
/// through to today's numeric parsing, which handles every such input.
fn detect_wall_clock(input: &str) -> Option<DetectedTime<'_>> {
    detect_epoch(input).or_else(|| detect_datetime(input))
}

/// Recognizes bare integers tagged `utc`/`unix`/`epoch`.
///
/// Tried before the datetime grammar on purpose: `1234 utc` would otherwise
/// read as the time `12:34 UTC`; the tagged-integer reading wins. Grouped
/// numbers (`1,234 utc`) are rejected so they keep today's numeric behavior.
fn detect_epoch(input: &str) -> Option<DetectedTime<'static>> {
    // chrono's representable range is years 1..=9999; anything outside falls
    // back to the numeric path instead of erroring mid-conversion later.
    const MIN_EPOCH_SECONDS: i64 = -62_135_596_800;
    const MAX_EPOCH_SECONDS: i64 = 253_402_300_799;

    let (number, tag) = input.split_once(char::is_whitespace)?;
    let digits = number.strip_prefix(['+', '-']).unwrap_or(number);
    if digits.is_empty() || digits.len() > 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !matches!(
        tag.trim().to_ascii_lowercase().as_str(),
        "utc" | "unix" | "epoch"
    ) {
        return None;
    }

    let seconds: i64 = number.parse().ok()?;
    if !(MIN_EPOCH_SECONDS..=MAX_EPOCH_SECONDS).contains(&seconds) {
        return None;
    }

    Some(DetectedTime {
        moment: WallClock {
            epoch_seconds: seconds,
            source_zone: ResolvedZone::Utc,
        },
        target: None,
    })
}

fn detect_datetime(input: &str) -> Option<DetectedTime<'_>> {
    let mut cursor = input;
    let date = take_date(&mut cursor);
    if date.is_some() {
        // A full date demands a time right after it (`T`, `t`, or a space);
        // date-only inputs stay on the numeric path per the tight scope.
        let separator = cursor.chars().next()?;
        if !matches!(separator, 'T' | 't' | ' ') {
            return None;
        }
        cursor = &cursor[separator.len_utf8()..];
    }

    let time = take_time(&mut cursor)?;

    let mut source_zone = ResolvedZone::Local;
    let mut rest = cursor.trim_start();
    if !rest.is_empty() && take_target_clause(rest).is_none() {
        let (token, remainder) = split_zone_token(rest);
        let zone = resolve_zone(token)?;
        source_zone = zone;
        rest = remainder.trim_start();
    }

    let target = if rest.is_empty() {
        None
    } else {
        Some(take_target_clause(rest)?)
    };

    let naive = match date {
        Some(day) => day.and_hms_opt(time.hour, time.minute, time.second)?,
        None => today_in_zone(source_zone).and_hms_opt(time.hour, time.minute, time.second)?,
    };
    let epoch_seconds = match source_zone {
        ResolvedZone::Utc => naive.and_utc().timestamp(),
        ResolvedZone::Fixed(offset) => resolve_in_zone(&offset, naive).timestamp(),
        ResolvedZone::Tz(zone) => resolve_in_zone(&zone, naive).timestamp(),
        ResolvedZone::Local => resolve_in_zone(&Local, naive).timestamp(),
    };

    Some(DetectedTime {
        moment: WallClock {
            epoch_seconds,
            source_zone,
        },
        target,
    })
}

/// Consumes a leading `YYYY-MM-DD` prefix when present.
fn take_date(cursor: &mut &str) -> Option<NaiveDate> {
    let bytes = cursor.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    for (idx, byte) in bytes.iter().take(10).enumerate() {
        let expected = match idx {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        };
        if !expected {
            return None;
        }
    }
    let year: i32 = cursor[..4].parse().ok()?;
    let month: u32 = cursor[5..7].parse().ok()?;
    let day: u32 = cursor[8..10].parse().ok()?;
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    *cursor = &cursor[10..];
    Some(date)
}

/// Time-of-day fields scanned off a `HH:MM[:SS][.frac]` prefix.
struct TimeParts {
    hour: u32,
    minute: u32,
    second: u32,
}

/// Consumes a leading strict two-digit `HH:MM[:SS]` prefix, dropping any
/// fractional seconds (they never change the second-truncated result).
///
/// Out-of-range components (`25:99`) yield `None` so the caller falls back to
/// numeric parsing instead of inventing a bogus instant.
fn take_time(cursor: &mut &str) -> Option<TimeParts> {
    let bytes = cursor.as_bytes();
    let shaped = bytes.len() >= 5
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b':'
        && bytes[3].is_ascii_digit()
        && bytes[4].is_ascii_digit();
    if !shaped {
        return None;
    }

    let hour: u32 = cursor[..2].parse().ok()?;
    let minute: u32 = cursor[3..5].parse().ok()?;
    let mut second = 0;
    let mut consumed = 5;
    if bytes.get(5) == Some(&b':')
        && bytes.get(6).is_some_and(u8::is_ascii_digit)
        && bytes.get(7).is_some_and(u8::is_ascii_digit)
    {
        second = cursor[6..8].parse().ok()?;
        consumed = 8;
        // Fractional seconds are consumed and truncated; they never change
        // the integer-second instant this payload carries.
        if bytes.get(consumed) == Some(&b'.') {
            let mut end = consumed;
            while bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end > consumed {
                consumed = end + 1;
            }
        }
    }

    if hour >= 24 || minute >= 60 || second >= 60 {
        return None;
    }
    *cursor = &cursor[consumed..];
    Some(TimeParts {
        hour,
        minute,
        second,
    })
}

/// Splits one whitespace-delimited token off the front of `rest`.
fn split_zone_token(rest: &str) -> (&str, &str) {
    rest.find(char::is_whitespace)
        .map_or((rest, ""), |idx| (&rest[..idx], &rest[idx..]))
}

/// Case-insensitively strips an ASCII clause prefix (`to `, `in `, `-> `).
///
/// The `→ ` arrow prefix is non-ASCII but safe: the char-boundary check makes
/// the slice legal, and `eq_ignore_ascii_case` compares those bytes exactly.
fn strip_ascii_prefix<'a>(rest: &'a str, prefix: &str) -> Option<&'a str> {
    if rest.len() >= prefix.len()
        && rest.is_char_boundary(prefix.len())
        && rest[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        Some(&rest[prefix.len()..])
    } else {
        None
    }
}

/// Matches a trailing target clause and returns the zone token it names.
///
/// The target must be a single token because IANA ids and fixed offsets never
/// contain whitespace; multi-word matches are treated as junk so detection
/// bails instead of guessing.
fn take_target_clause(rest: &str) -> Option<&str> {
    for prefix in ["to ", "in ", "-> ", "→ "] {
        if let Some(after) = strip_ascii_prefix(rest, prefix) {
            let target = after.trim();
            return (!target.is_empty() && !target.contains(char::is_whitespace)).then_some(target);
        }
    }
    None
}

/// Zone abbreviations that the IANA database does not expose as zone ids under
/// their short name.
///
/// Each maps to one representative zone; the choice favors the largest
/// population sharing the abbreviation and is deliberately DST-aware, so
/// `EST` follows New York rather than the legacy fixed-offset `EST` link.
/// Ambiguity is accepted and documented instead of erroring (`CST` means US
/// Central, not China; `IST` means India, not Ireland or Israel).
const ZONE_ABBREVIATIONS: &[(&str, &str)] = &[
    ("est", "America/New_York"),
    ("edt", "America/New_York"),
    ("cst", "America/Chicago"),
    ("cdt", "America/Chicago"),
    ("mst", "America/Denver"),
    ("mdt", "America/Denver"),
    ("pst", "America/Los_Angeles"),
    ("pdt", "America/Los_Angeles"),
    ("akst", "America/Anchorage"),
    ("akdt", "America/Anchorage"),
    ("brt", "America/Sao_Paulo"),
    ("art", "America/Argentina/Buenos_Aires"),
    ("clt", "America/Santiago"),
    ("cest", "Europe/Berlin"),
    ("bst", "Europe/London"),
    ("msk", "Europe/Moscow"),
    ("ist", "Asia/Kolkata"),
    ("pkt", "Asia/Karachi"),
    ("gst", "Asia/Dubai"),
    ("jst", "Asia/Tokyo"),
    ("kst", "Asia/Seoul"),
    ("sgt", "Asia/Singapore"),
    ("hkt", "Asia/Hong_Kong"),
    ("aest", "Australia/Sydney"),
    ("aedt", "Australia/Sydney"),
    ("acst", "Australia/Adelaide"),
    ("awst", "Australia/Perth"),
    ("nzst", "Pacific/Auckland"),
    ("nzdt", "Pacific/Auckland"),
];

/// Resolves a zone designator token for both source zones and explicit targets.
///
/// Accepts `UTC`/`GMT`/`Z`, fixed offsets (`+05:30`, `-0700`, `+02`), IANA ids
/// (case-insensitive via chrono-tz), and common abbreviations from
/// [`ZONE_ABBREVIATIONS`]. Returns `None` for anything unrecognized so callers
/// can fall back or report a clear error.
#[must_use]
pub fn resolve_zone(token: &str) -> Option<ResolvedZone> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    if matches!(token.to_ascii_lowercase().as_str(), "utc" | "z" | "gmt") {
        return Some(ResolvedZone::Utc);
    }
    if let Some(offset) = parse_fixed_offset(token) {
        return Some(ResolvedZone::Fixed(offset));
    }
    if let Some((_, id)) = ZONE_ABBREVIATIONS
        .iter()
        .find(|(abbr, _)| *abbr == token.to_ascii_lowercase())
    {
        return id.parse::<Tz>().ok().map(ResolvedZone::Tz);
    }
    token.parse::<Tz>().ok().map(ResolvedZone::Tz)
}

/// Parses `±HH:MM`, `±HHMM`, or `±HH` into a fixed offset.
///
/// Offsets at or beyond ±24 hours are rejected by `east_opt` itself.
fn parse_fixed_offset(token: &str) -> Option<FixedOffset> {
    let sign = match token.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = &token[1..];
    let (hours, minutes) = if let Some((h, m)) = rest.split_once(':') {
        (h, m)
    } else if rest.len() == 4 {
        (&rest[..2], &rest[2..])
    } else if rest.len() == 2 {
        (rest, "00")
    } else {
        return None;
    };
    if hours.len() != 2
        || minutes.len() != 2
        || !hours.bytes().all(|b| b.is_ascii_digit())
        || !minutes.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let total = hours.parse::<i32>().ok()? * 3600 + minutes.parse::<i32>().ok()? * 60;
    FixedOffset::east_opt(sign * total)
}

/// Today's calendar date as seen in `zone`.
fn today_in_zone(zone: ResolvedZone) -> NaiveDate {
    match zone {
        ResolvedZone::Utc => Utc::now().date_naive(),
        ResolvedZone::Local => Local::now().date_naive(),
        ResolvedZone::Fixed(offset) => Utc::now().with_timezone(&offset).date_naive(),
        ResolvedZone::Tz(tz) => Utc::now().with_timezone(&tz).date_naive(),
    }
}

/// Resolves a wall-clock reading in `zone` to one concrete instant, never
/// failing on DST edges:
///
/// - unambiguous readings map directly,
/// - ambiguous readings (autumn overlap) take the **earlier** instant,
/// - skipped readings (spring gap) shift forward in 30-minute steps until the
///   reading lands on a valid local time, i.e. they resolve to the first wall
///   clock time at or after the gap ends.
///
/// The final `from_utc_datetime` fallback is unreachable for real zones but
/// keeps the function total.
fn resolve_in_zone<Z: TimeZone>(zone: &Z, reading: NaiveDateTime) -> DateTime<Z> {
    if let Some(resolved) = zone.from_local_datetime(&reading).single() {
        return resolved;
    }
    if let Some(earlier) = zone.from_local_datetime(&reading).earliest() {
        return earlier;
    }
    for shift_minutes in [30, 60, 90, 120, 150, 180, 210, 240] {
        let shifted = reading + Duration::minutes(shift_minutes);
        if let Some(resolved) = zone.from_local_datetime(&shifted).earliest() {
            return resolved;
        }
    }
    zone.from_utc_datetime(&reading)
}

fn scan_number_end(core_input: &str) -> (usize, bool) {
    let mut number_end = 0;
    let mut found_digit = false;
    let mut found_e = false;
    let mut last_char_was_e = false;

    for (i, c) in core_input.char_indices() {
        if c.is_ascii_digit() {
            found_digit = true;
            number_end = i + 1;
            last_char_was_e = false;
        } else if (c == '.' || c == ',') && !found_e {
            number_end = i + 1;
            last_char_was_e = false;
        } else if (c == 'e' || c == 'E')
            && found_digit
            && !found_e
            && exponent_continues(&core_input[i + c.len_utf8()..])
        {
            found_e = true;
            last_char_was_e = true;
            number_end = i + 1;
        } else if (c == '+' || c == '-') && last_char_was_e {
            number_end = i + 1;
            last_char_was_e = false;
        } else if c.is_whitespace() {
            let remaining = &core_input[i + 1..];
            let mut is_part_of_number = false;
            for nc in remaining.chars() {
                let exponent = matches!(nc, 'e' | 'E')
                    && !found_e
                    && exponent_continues(remaining.trim_start().get(1..).unwrap_or(""));
                if nc.is_ascii_digit()
                    || ((nc == '.' || nc == ',') && !found_e)
                    || exponent
                    || ((nc == '+' || nc == '-') && last_char_was_e)
                {
                    is_part_of_number = true;
                    break;
                } else if !nc.is_whitespace() {
                    break;
                }
            }
            if is_part_of_number {
                continue;
            }
            break;
        } else if c.is_alphabetic() || c == '%' {
            break;
        } else if c == '-' && !found_digit {
            number_end = i + 1;
        } else {
            break;
        }
    }

    (number_end, found_digit)
}

fn exponent_continues(after_e: &str) -> bool {
    let mut chars = after_e.chars();
    match chars.next() {
        Some('+' | '-') => chars.next().is_some_and(|c| c.is_ascii_digit()),
        Some(c) if c.is_ascii_digit() => true,
        _ => false,
    }
}

/// Detects `<start>-<end> <unit>` / `<start> to <end> <unit>` shapes and
/// returns `(start, end, unit_text)`.
///
/// Every candidate separator must have plain numbers on both sides and leave a
/// non-empty unit-like remainder; otherwise the caller falls through to the
/// single-value path instead of erroring. This keeps dates (`2020-01-02`),
/// versions (`1.2.3`), negative singles (`-5 USD`) and times (`17:30 UTC`) on
/// their existing behavior.
fn split_range(core_input: &str) -> Option<(f64, f64, &str)> {
    // A `-` counts as a separator only when squeezed directly between two
    // digits, so spaced dashes and signed values stay out of range territory.
    let mut previous_was_digit = false;
    for (i, c) in core_input.char_indices() {
        let next_is_digit = core_input[i + c.len_utf8()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_ascii_digit());
        if c == '-'
            && previous_was_digit
            && next_is_digit
            && let Some(found) = range_parts(&core_input[..i], &core_input[i + 1..])
        {
            return Some(found);
        }
        previous_was_digit = c.is_ascii_digit();
    }

    for (word_start, word_end) in standalone_to_word_spans(core_input) {
        if let Some(found) = range_parts(&core_input[..word_start], &core_input[word_end..]) {
            return Some(found);
        }
    }

    None
}

/// Validates one candidate range split: both sides must be fully numeric and
/// the remainder after the second number must look like a unit, not like more
/// numeric input (which would mean a date such as `2020-01-02`).
fn range_parts<'a>(left: &str, right: &'a str) -> Option<(f64, f64, &'a str)> {
    let start = parse_plain_number(left.trim())?;

    let (number_end, found_digit) = scan_number_end(right);
    if !found_digit {
        return None;
    }
    let end = parse_plain_number(right[..number_end].trim())?;

    let unit_text = right[number_end..].trim();
    let unit_like = unit_text
        .chars()
        .next()
        .is_some_and(|first| !first.is_ascii_digit() && !matches!(first, '+' | '-' | '.' | ','));
    if !unit_like {
        return None;
    }

    Some((start, end, unit_text))
}

/// Parses a complete token as a plain (possibly grouped) number, returning
/// `None` for anything partially numeric so range detection can fall back.
fn parse_plain_number(token: &str) -> Option<f64> {
    normalize_numeric(token)?.parse::<f64>().ok()
}

/// Finds byte spans of standalone `to`/`TO` words surrounded by whitespace.
///
/// Scans ASCII bytes directly because lowercasing the whole string can change
/// character lengths for some Unicode inputs, which would misalign indices cut
/// from the original string.
fn standalone_to_word_spans(input: &str) -> Vec<(usize, usize)> {
    let bytes = input.as_bytes();
    let mut spans = Vec::new();
    for (i, &byte) in bytes.iter().enumerate() {
        let standalone_to = matches!(byte, b't' | b'T')
            && i > 0
            && bytes[i - 1].is_ascii_whitespace()
            && i + 2 < bytes.len()
            && matches!(bytes[i + 1], b'o' | b'O')
            && bytes[i + 2].is_ascii_whitespace();
        if standalone_to {
            spans.push((i, i + 2));
        }
    }
    spans
}

/// Turns grouped/locale number text into something `f64::parse` accepts.
///
/// Returns `None` for tokens that mix an exponent marker with a comma
/// (`1,5e3`): reading the comma as grouping yields 15000 while reading it as a
/// decimal separator yields 1500, so the caller must reject the token instead
/// of converting a silently wrong magnitude.
fn normalize_numeric(raw: &str) -> Option<String> {
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    // A `,` next to an exponent is never safely interpretable on its own, so
    // refuse to guess even when it could plausibly be grouping (`1,234e5`).
    if compact.contains(',') && (compact.contains('e') || compact.contains('E')) {
        return None;
    }

    if compact.contains('e') || compact.contains('E') {
        return Some(compact);
    }

    let last_comma = compact.rfind(',');
    let last_dot = compact.rfind('.');
    Some(match (last_comma, last_dot) {
        (Some(c), Some(d)) if d > c => compact.replace(',', ""),
        (Some(_), Some(_)) => compact.replace('.', "").replace(',', "."),
        (Some(_), None) => {
            let parts: Vec<&str> = compact.split(',').collect();
            if parts.len() == 2 && parts[1].len() != 3 {
                format!("{}.{}", parts[0], parts[1])
            } else if parts.iter().skip(1).all(|p| p.len() == 3) {
                compact.replace(',', "")
            } else {
                compact.replace(',', ".")
            }
        }
        (None, Some(_)) => {
            let parts: Vec<&str> = compact.split('.').collect();
            if parts.len() > 2 && parts.iter().skip(1).all(|p| p.len() == 3) {
                compact.replace('.', "")
            } else {
                compact
            }
        }
        _ => compact,
    })
}

/// Resolves leftover text after the number into a source unit and optional
/// explicit target, folding in a currency glyph when the text does not name a
/// currency itself.
///
/// Shared by the single-value and range paths so both build units identically.
fn finalize_unit_and_target(
    unit_str: &str,
    mut symbol_unit: Option<&'static str>,
) -> (Option<String>, Option<String>) {
    let mut unit_str = unit_str.trim();
    if symbol_unit.is_none()
        && let Some((rest, unit)) = strip_trailing_currency(unit_str)
    {
        unit_str = rest;
        symbol_unit = Some(unit);
    }

    let (source, target) = split_source_target(unit_str);
    // `$100/hr`: a leading currency glyph followed by a bare `/rate` fragment
    // joins into one compound unit (`USD/hr`) instead of appending the
    // currency behind the slash (`/hr USD`).
    if let Some(sym) = symbol_unit
        && let Some(rate) = source.as_deref().filter(|s| s.starts_with('/'))
    {
        return (Some(format!("{sym}{rate}")), target);
    }
    let unit = match (source.as_deref(), symbol_unit) {
        (None, None) => None,
        (Some(s), None) => Some(s.to_string()),
        (None, Some(sym)) => Some(sym.to_string()),
        (Some(s), Some(sym)) => {
            if s.eq_ignore_ascii_case(sym)
                || s.to_lowercase().ends_with(sym.to_lowercase().as_str())
            {
                Some(s.to_string())
            } else {
                Some(format!("{s} {sym}"))
            }
        }
    };
    (unit, target)
}

fn strip_trailing_currency(unit_str: &str) -> Option<(&str, &'static str)> {
    let trimmed = unit_str.trim_end();
    for (sym, unit) in CURRENCY_SYMBOLS {
        if let Some(rest) = trimmed.strip_suffix(sym) {
            return Some((rest.trim_end(), unit));
        }
    }
    None
}

fn split_source_target(unit_str: &str) -> (Option<String>, Option<String>) {
    let trimmed = unit_str.trim();
    if trimmed.is_empty() {
        return (None, None);
    }

    let lower = trimmed.to_lowercase();
    for sep in [" to ", " in ", " -> ", " → "] {
        if let Some(idx) = lower.find(sep) {
            return (
                nonempty(&trimmed[..idx]),
                nonempty(&trimmed[idx + sep.len()..]),
            );
        }
    }
    for prefix in ["to ", "in ", "-> ", "→ "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let start = trimmed.len().saturating_sub(rest.len());
            return (None, nonempty(&trimmed[start..]));
        }
    }
    (nonempty(trimmed), None)
}

fn nonempty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;
    use chrono::Timelike;

    #[test]
    fn test_parse_plain_number() {
        let res = parse_input("123.45").unwrap();
        assert_eq!(res.value, 123.45);
        assert_eq!(res.unit, None);
        assert_eq!(res.target, None);
    }

    #[test]
    fn test_parse_currency_symbol() {
        let res = parse_input("$50").unwrap();
        assert_eq!(res.value, 50.0);
        assert_eq!(res.unit, Some("USD".to_string()));

        let res = parse_input("€ 120.50").unwrap();
        assert_eq!(res.value, 120.50);
        assert_eq!(res.unit, Some("EUR".to_string()));

        let res = parse_input("$ 100 000").unwrap();
        assert_eq!(res.value, 100_000.0);
        assert_eq!(res.unit, Some("USD".to_string()));
    }

    #[test]
    fn test_parse_number_with_unit() {
        let res = parse_input("10kg").unwrap();
        assert_eq!(res.value, 10.0);
        assert_eq!(res.unit, Some("kg".to_string()));

        let res = parse_input("20.5  meters").unwrap();
        assert_eq!(res.value, 20.5);
        assert_eq!(res.unit, Some("meters".to_string()));
    }

    #[test]
    fn test_parse_negative_number() {
        let res = parse_input("-15.2").unwrap();
        assert_eq!(res.value, -15.2);
        assert_eq!(res.unit, None);
    }

    #[test]
    fn test_parse_number_with_spaces() {
        let res = parse_input("100 000 USD").unwrap();
        assert_eq!(res.value, 100_000.0);
        assert_eq!(res.unit, Some("USD".to_string()));
    }

    #[test]
    fn test_parse_invalid_input() {
        assert!(parse_input("abc").is_err());
        assert!(parse_input("").is_err());
        assert!(parse_input("$").is_err());
    }

    #[test]
    fn dollar_glyph_with_rate_fragment_joins_the_currency() {
        let res = parse_input("$100/hr").unwrap();
        assert_eq!(res.value, 100.0);
        assert_eq!(res.unit, Some("USD/hr".to_string()));
    }

    #[test]
    fn compound_source_and_target_parse_without_a_glyph() {
        let res = parse_input("10 USD/kg to USD/lb").unwrap();
        assert_eq!(res.value, 10.0);
        assert_eq!(res.unit, Some("USD/kg".to_string()));
        assert_eq!(res.target, Some("USD/lb".to_string()));
    }

    #[test]
    fn counted_denominator_passes_through_to_the_converter() {
        let res = parse_input("100 kWh/100km").unwrap();
        assert_eq!(res.value, 100.0);
        assert_eq!(res.unit, Some("kWh/100km".to_string()));
    }

    #[test]
    fn test_parse_scientific_notation() {
        let res = parse_input("1e-9 meters").unwrap();
        assert_eq!(res.value, 1e-9);
        assert_eq!(res.unit, Some("meters".to_string()));

        let res = parse_input("1.5E3 USD").unwrap();
        assert_eq!(res.value, 1500.0);
        assert_eq!(res.unit, Some("USD".to_string()));

        let res = parse_input("-2.5e+4").unwrap();
        assert_eq!(res.value, -25000.0);
        assert_eq!(res.unit, None);
    }

    #[test]
    fn test_parse_symbol_with_multiplier() {
        let res = parse_input("$100B").unwrap();
        assert_eq!(res.value, 100.0);
        assert_eq!(res.unit, Some("B USD".to_string()));

        let res = parse_input("$ 39.6 BILLION").unwrap();
        assert_eq!(res.value, 39.6);
        assert_eq!(res.unit, Some("BILLION USD".to_string()));

        let res = parse_input("€1.5M").unwrap();
        assert_eq!(res.value, 1.5);
        assert_eq!(res.unit, Some("M EUR".to_string()));
    }

    #[test]
    fn parse_should_accept_comma_thousands() {
        let res = parse_input("1,234.56 USD").unwrap();
        assert_eq!(res.value, 1234.56);
        assert_eq!(res.unit, Some("USD".to_string()));

        let res = parse_input("$1,000").unwrap();
        assert_eq!(res.value, 1000.0);
        assert_eq!(res.unit, Some("USD".to_string()));
    }

    #[test]
    fn parse_should_accept_european_decimal() {
        let res = parse_input("1.234,56 EUR").unwrap();
        assert_eq!(res.value, 1234.56);
        assert_eq!(res.unit, Some("EUR".to_string()));
    }

    #[test]
    fn parse_should_accept_trailing_currency_symbol() {
        let res = parse_input("100$").unwrap();
        assert_eq!(res.value, 100.0);
        assert_eq!(res.unit, Some("USD".to_string()));
    }

    #[test]
    fn parse_should_accept_comma_decimal_without_exponent() {
        let res = parse_input("1,5").unwrap();
        assert_eq!(res.value, 1.5);
        assert_eq!(res.unit, None);

        let res = parse_input("1,5 USD").unwrap();
        assert_eq!(res.value, 1.5);
        assert_eq!(res.unit, Some("USD".to_string()));
    }

    #[test]
    fn parse_should_accept_dot_decimal_with_exponent() {
        let res = parse_input("1.5e3").unwrap();
        assert_eq!(res.value, 1500.0);

        let res = parse_input("15000").unwrap();
        assert_eq!(res.value, 15_000.0);
    }

    #[test]
    fn parse_should_accept_grouped_digits_with_fraction() {
        let res = parse_input("1,234.5 USD").unwrap();
        assert_eq!(res.value, 1234.5);
        assert_eq!(res.unit, Some("USD".to_string()));
    }

    #[test]
    fn parse_should_reject_comma_combined_with_exponent() {
        // `1,5e3` used to silently become `15e3` = 15000; ambiguity must be an
        // error rather than a wrong conversion result.
        assert!(parse_input("1,5e3").is_err());
        assert!(parse_input("1,5E3 USD").is_err());
        // Even a plausible grouping (`1,234e5`) is refused next to an exponent.
        assert!(parse_input("1,234e5").is_err());
    }

    #[test]
    fn parse_should_split_to_target() {
        let res = parse_input("100 USD to PLN").unwrap();
        assert_eq!(res.value, 100.0);
        assert_eq!(res.unit, Some("USD".to_string()));
        assert_eq!(res.target, Some("PLN".to_string()));

        let res = parse_input("$100 to EUR").unwrap();
        assert_eq!(res.value, 100.0);
        assert_eq!(res.unit, Some("USD".to_string()));
        assert_eq!(res.target, Some("EUR".to_string()));
    }

    #[test]
    fn parse_should_split_target_regardless_of_case() {
        let res = parse_input("100 usd to pln").unwrap();
        assert_eq!(res.value, 100.0);
        assert_eq!(res.unit, Some("usd".to_string()));
        assert_eq!(res.target, Some("pln".to_string()));

        let res = parse_input("100 Usd To Pln").unwrap();
        assert_eq!(res.unit, Some("Usd".to_string()));
        assert_eq!(res.target, Some("Pln".to_string()));

        let res = parse_input("$100 -> eur").unwrap();
        assert_eq!(res.target, Some("eur".to_string()));
    }

    #[test]
    fn parse_should_detect_dash_range_with_explicit_unit() {
        let res = parse_input("10-20 USD").unwrap();
        assert_eq!(res.value, 10.0);
        assert_eq!(res.end_value, Some(20.0));
        assert_eq!(res.unit, Some("USD".to_string()));
        assert_eq!(res.target, None);
    }

    #[test]
    fn parse_should_detect_spelled_to_range_when_unit_follows() {
        let res = parse_input("10 to 20 USD").unwrap();
        assert_eq!(res.value, 10.0);
        assert_eq!(res.end_value, Some(20.0));
        assert_eq!(res.unit, Some("USD".to_string()));

        let res = parse_input("10 TO 20 USD").unwrap();
        assert_eq!(res.value, 10.0);
        assert_eq!(res.end_value, Some(20.0));
        assert_eq!(res.unit, Some("USD".to_string()));
    }

    #[test]
    fn parse_should_accept_grouped_digits_in_ranges() {
        let res = parse_input("1,000-2,000 USD").unwrap();
        assert_eq!(res.value, 1000.0);
        assert_eq!(res.end_value, Some(2000.0));
        assert_eq!(res.unit, Some("USD".to_string()));

        let res = parse_input("1 000 to 2 000 USD").unwrap();
        assert_eq!(res.value, 1000.0);
        assert_eq!(res.end_value, Some(2000.0));
        assert_eq!(res.unit, Some("USD".to_string()));
    }

    #[test]
    fn parse_should_accept_exponent_endpoints_in_range() {
        let res = parse_input("1e3-2e3 m").unwrap();
        assert_eq!(res.value, 1000.0);
        assert_eq!(res.end_value, Some(2000.0));
        assert_eq!(res.unit, Some("m".to_string()));

        let res = parse_input("1e-3-2e-3 m").unwrap();
        assert_eq!(res.value, 0.001);
        assert_eq!(res.end_value, Some(0.002));
        assert_eq!(res.unit, Some("m".to_string()));
    }

    #[test]
    fn parse_should_keep_target_clause_on_detected_range() {
        let res = parse_input("10-20 USD to PLN").unwrap();
        assert_eq!(res.value, 10.0);
        assert_eq!(res.end_value, Some(20.0));
        assert_eq!(res.unit, Some("USD".to_string()));
        assert_eq!(res.target, Some("PLN".to_string()));
    }

    #[test]
    fn parse_should_not_mistake_target_clause_for_range() {
        let res = parse_input("100 USD to PLN").unwrap();
        assert_eq!(res.value, 100.0);
        assert_eq!(res.end_value, None);
        assert_eq!(res.unit, Some("USD".to_string()));
        assert_eq!(res.target, Some("PLN".to_string()));
    }

    #[test]
    fn parse_should_fall_back_to_target_clause_when_to_lacks_second_number() {
        let res = parse_input("10 to PLN").unwrap();
        assert_eq!(res.value, 10.0);
        assert_eq!(res.end_value, None);
        assert_eq!(res.unit, None);
        assert_eq!(res.target, Some("PLN".to_string()));
    }

    #[test]
    fn parse_should_not_trigger_range_on_date_like_input() {
        // Falls through to today's behavior: first token becomes the value,
        // the rest becomes a bogus unit that the converter rejects.
        let res = parse_input("2020-01-02").unwrap();
        assert_eq!(res.value, 2020.0);
        assert_eq!(res.end_value, None);
        assert_eq!(res.unit, Some("-01-02".to_string()));
    }

    #[test]
    fn parse_should_not_trigger_range_on_version_like_input() {
        // Same failures as before ranges existed; importantly no range result.
        assert!(parse_input("1.2.3").is_err());
        assert!(parse_input("1.2.3-4.5.6").is_err());
    }

    #[test]
    fn parse_should_keep_negative_single_value_out_of_range_detection() {
        let res = parse_input("-5 USD").unwrap();
        assert_eq!(res.value, -5.0);
        assert_eq!(res.end_value, None);
        assert_eq!(res.unit, Some("USD".to_string()));
    }

    #[test]
    fn parse_should_detect_time_like_input_as_wall_clock() {
        // Used to fall through numerically (`17.0` + unit `:30 UTC`) before
        // wall-clock detection existed; #14 claims this shape.
        let res = parse_input("17:30 UTC").unwrap();
        let moment = res.wall_clock.expect("wall-clock payload");
        assert_eq!(moment.source_zone, ResolvedZone::Utc);
        assert_eq!(res.unit, Some("17:30 UTC".to_string()));
    }

    #[test]
    fn parse_should_require_explicit_unit_after_dash_range() {
        // Bare endpoints have nothing to convert; keep the old fall-through.
        let res = parse_input("10-20").unwrap();
        assert_eq!(res.value, 10.0);
        assert_eq!(res.end_value, None);
        assert_eq!(res.unit, Some("-20".to_string()));
    }

    #[test]
    fn parse_should_fall_through_when_range_side_is_not_numeric() {
        let res = parse_input("10-twenty USD").unwrap();
        assert_eq!(res.value, 10.0);
        assert_eq!(res.end_value, None);
        assert_eq!(res.unit, Some("-twenty USD".to_string()));
    }

    #[test]
    fn parse_should_support_currency_glyphs_in_ranges() {
        let res = parse_input("$10-20 USD").unwrap();
        assert_eq!(res.value, 10.0);
        assert_eq!(res.end_value, Some(20.0));
        assert_eq!(res.unit, Some("USD".to_string()));

        let res = parse_input("10-20$").unwrap();
        assert_eq!(res.value, 10.0);
        assert_eq!(res.end_value, Some(20.0));
        assert_eq!(res.unit, Some("USD".to_string()));
    }

    #[test]
    fn parse_should_detect_rfc3339_zulu_datetime() {
        let res = parse_input("2026-08-21T14:00Z").unwrap();
        let moment = res.wall_clock.expect("wall-clock payload");
        let expected = Utc
            .with_ymd_and_hms(2026, 8, 21, 14, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(moment.epoch_seconds, expected);
        assert_eq!(moment.source_zone, ResolvedZone::Utc);
        assert_eq!(res.target, None);
    }

    #[test]
    fn parse_should_detect_rfc3339_fixed_offset_datetime() {
        let res = parse_input("2026-08-21T14:00:00+02:00").unwrap();
        let moment = res.wall_clock.expect("wall-clock payload");
        // 14:00 at +02:00 is the same instant as 12:00 UTC.
        let expected = Utc
            .with_ymd_and_hms(2026, 8, 21, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(moment.epoch_seconds, expected);
        assert_eq!(
            moment.source_zone,
            ResolvedZone::Fixed(parse_fixed_offset("+02:00").unwrap())
        );
    }

    #[test]
    fn parse_should_accept_spaced_iso_with_zone_name() {
        // Winter date so the expectation holds even if DST rules change.
        let res = parse_input("2026-01-15 14:00 Europe/Warsaw").unwrap();
        let moment = res.wall_clock.expect("wall-clock payload");
        assert_eq!(
            moment.source_zone,
            ResolvedZone::Tz("Europe/Warsaw".parse::<Tz>().unwrap())
        );
        let expected = Utc
            .with_ymd_and_hms(2026, 1, 15, 13, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(moment.epoch_seconds, expected);
    }

    #[test]
    fn parse_should_truncate_fractional_seconds() {
        let res = parse_input("2026-08-21T14:00:00.75Z").unwrap();
        let moment = res.wall_clock.expect("wall-clock payload");
        let expected = Utc
            .with_ymd_and_hms(2026, 8, 21, 14, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(moment.epoch_seconds, expected);
    }

    #[test]
    fn parse_should_detect_glued_zulu_designator() {
        let res = parse_input("17:30Z").unwrap();
        let moment = res.wall_clock.expect("wall-clock payload");
        assert_eq!(moment.source_zone, ResolvedZone::Utc);
        let rendered = Utc.timestamp_opt(moment.epoch_seconds, 0).single().unwrap();
        assert_eq!((rendered.hour(), rendered.minute()), (17, 30));
    }

    #[test]
    fn parse_should_treat_untagged_time_fragments_as_local() {
        let res = parse_input("09:15").unwrap();
        let moment = res.wall_clock.expect("wall-clock payload");
        assert_eq!(moment.source_zone, ResolvedZone::Local);
        let rendered = Local
            .timestamp_opt(moment.epoch_seconds, 0)
            .single()
            .unwrap();
        assert_eq!((rendered.hour(), rendered.minute()), (9, 15));
    }

    #[test]
    fn parse_should_detect_tagged_epoch_integers() {
        let res = parse_input("1755792000 utc").unwrap();
        let moment = res.wall_clock.expect("wall-clock payload");
        assert_eq!(moment.epoch_seconds, 1_755_792_000);
        assert_eq!(moment.source_zone, ResolvedZone::Utc);
        assert_eq!(res.value, 1_755_792_000.0);

        let res = parse_input("42 UNIX").unwrap();
        assert_eq!(
            res.wall_clock.expect("wall-clock payload").epoch_seconds,
            42
        );

        let res = parse_input("-86400 epoch").unwrap();
        assert_eq!(
            res.wall_clock.expect("wall-clock payload").epoch_seconds,
            -86_400
        );
    }

    #[test]
    fn parse_should_carry_target_clause_on_wall_clock_inputs() {
        let res = parse_input("17:30 UTC to CET").unwrap();
        let moment = res.wall_clock.expect("wall-clock payload");
        assert_eq!(moment.source_zone, ResolvedZone::Utc);
        assert_eq!(res.target, Some("CET".to_string()));

        let res = parse_input("17:30 in PST").unwrap();
        assert!(res.wall_clock.is_some());
        assert_eq!(res.target, Some("PST".to_string()));
    }

    #[test]
    fn parse_should_keep_durations_and_plain_numbers_off_the_wall_clock_path() {
        for input in ["30 min", "5 s", "15000", "100 USD"] {
            let res = parse_input(input).unwrap();
            assert!(res.wall_clock.is_none(), "{input} became wall clock");
        }
    }

    #[test]
    fn parse_should_fall_back_to_numeric_for_unknown_source_zone() {
        // Numerically valid text keeps today's behavior when the zone word is
        // not a known designator.
        let res = parse_input("17:30 XYZ").unwrap();
        assert!(res.wall_clock.is_none());
        assert_eq!(res.value, 17.0);
        assert_eq!(res.unit, Some(":30 XYZ".to_string()));
    }

    #[test]
    fn parse_should_fall_back_to_numeric_for_out_of_range_time_components() {
        let res = parse_input("25:99 UTC").unwrap();
        assert!(res.wall_clock.is_none());
        assert_eq!(res.value, 25.0);
        assert_eq!(res.unit, Some(":99 UTC".to_string()));
    }

    #[test]
    fn parse_should_keep_date_only_inputs_numeric() {
        // Tight scope: wall-clock detection requires a time component.
        let res = parse_input("2026-08-21").unwrap();
        assert!(res.wall_clock.is_none());
        assert_eq!(res.value, 2026.0);
        assert_eq!(res.unit, Some("-08-21".to_string()));
    }

    #[test]
    fn parse_should_reject_grouped_numbers_as_epoch() {
        let res = parse_input("1,234 utc").unwrap();
        assert!(res.wall_clock.is_none());
        assert_eq!(res.value, 1234.0);
        assert_eq!(res.unit, Some("utc".to_string()));
    }

    #[test]
    fn parse_should_keep_range_detection_ahead_of_wall_clock() {
        let res = parse_input("10-20 utc").unwrap();
        assert!(res.wall_clock.is_none());
        assert_eq!(res.value, 10.0);
        assert_eq!(res.end_value, Some(20.0));
        assert_eq!(res.unit, Some("utc".to_string()));
    }

    #[test]
    fn parse_should_resolve_dst_ambiguous_time_to_the_earlier_instant() {
        // 2025-10-26 02:30 happens twice in Warsaw (CEST then CET); detection
        // must pick deterministically instead of erroring or guessing later.
        let res = parse_input("2025-10-26 02:30 Europe/Warsaw").unwrap();
        let moment = res.wall_clock.expect("wall-clock payload");
        let expected = Utc
            .with_ymd_and_hms(2025, 10, 26, 0, 30, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(moment.epoch_seconds, expected);
    }

    #[test]
    fn parse_should_shift_dst_skipped_time_to_the_first_valid_reading() {
        // 2025-03-30 02:30 never happened in Warsaw (clocks jumped to 03:00);
        // the reading resolves forward onto 03:00 CET == 01:00 UTC.
        let res = parse_input("2025-03-30 02:30 Europe/Warsaw").unwrap();
        let moment = res.wall_clock.expect("wall-clock payload");
        let expected = Utc
            .with_ymd_and_hms(2025, 3, 30, 1, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(moment.epoch_seconds, expected);
    }
}
