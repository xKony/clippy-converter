use crate::models::ThousandSeparator;

/// Formats a number for on-screen display, with optional thousand grouping.
#[must_use]
pub fn format_display(value: f64, precision: usize, separator: ThousandSeparator) -> String {
    if !value.is_finite() {
        return value.to_string();
    }

    let formatted = format!("{value:.precision$}");
    let (negative, digits) = formatted
        .strip_prefix('-')
        .map_or((false, formatted.as_str()), |rest| (true, rest));

    let (integer, fraction) = digits
        .split_once('.')
        .map_or((digits, None), |(int_part, frac)| (int_part, Some(frac)));

    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&group_integer(integer, separator));
    if let Some(frac) = fraction {
        out.push('.');
        out.push_str(frac);
    }
    out
}

/// Formats a number for clipboard copy (no thousand separators).
#[must_use]
pub fn format_copy(value: f64) -> String {
    value.to_string()
}

fn group_integer(integer: &str, separator: ThousandSeparator) -> String {
    let sep = match separator {
        ThousandSeparator::None => return integer.to_string(),
        ThousandSeparator::Space => ' ',
        ThousandSeparator::Comma => ',',
    };

    let len = integer.len();
    if len <= 3 {
        return integer.to_string();
    }

    // `integer` is ASCII digits from `format!("{value:.precision$}")`.
    let remainder = len % 3;
    let mut out = String::with_capacity(len + len / 3);
    if remainder > 0 {
        out.push_str(&integer[..remainder]);
    }
    for chunk in integer.as_bytes()[remainder..].chunks(3) {
        if !out.is_empty() {
            out.push(sep);
        }
        if let Ok(part) = std::str::from_utf8(chunk) {
            out.push_str(part);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::models::ThousandSeparator;

    #[test]
    fn display_no_separator() {
        assert_eq!(format_display(1234.5, 1, ThousandSeparator::None), "1234.5");
    }

    #[test]
    fn display_space_separator() {
        assert_eq!(
            format_display(1_234_567.89, 2, ThousandSeparator::Space),
            "1 234 567.89"
        );
    }

    #[test]
    fn display_comma_separator() {
        assert_eq!(
            format_display(12_345.0, 1, ThousandSeparator::Comma),
            "12,345.0"
        );
    }

    #[test]
    fn display_negative() {
        assert_eq!(
            format_display(-1000.0, 0, ThousandSeparator::Space),
            "-1 000"
        );
    }

    #[test]
    fn copy_has_no_separators() {
        assert_eq!(format_copy(1_234_567.89), "1234567.89");
    }
}
