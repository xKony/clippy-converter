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
///
/// `precision` pins the decimal places (trailing zeros trimmed); `None`
/// keeps the full float repr.
#[must_use]
pub fn format_copy_precise(value: f64, precision: Option<usize>) -> String {
    let Some(precision) = precision else {
        return value.to_string();
    };
    if !value.is_finite() {
        return value.to_string();
    }

    let formatted = format!("{value:.precision$}");
    if formatted.contains('.') {
        let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
        return trimmed.to_string();
    }
    formatted
}

/// Formats a number for clipboard copy (no thousand separators).
#[must_use]
pub fn format_copy(value: f64) -> String {
    format_copy_precise(value, None)
}

/// Formats a value for the history log file (4 decimal places), falling back
/// to the full float repr when rounding would erase the value entirely
/// (e.g. tiny crypto amounts like `0.00001234 BTC`).
#[must_use]
pub fn format_history_value(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let formatted = format!("{value:.4}");
    if value != 0.0 && (formatted == "0.0000" || formatted == "-0.0000") {
        format_copy_precise(value, None)
    } else {
        formatted
    }
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

    #[test]
    fn copy_precision_rounds_and_trims_zeros() {
        assert_eq!(format_copy_precise(1_234.567_8, Some(2)), "1234.57");
        assert_eq!(format_copy_precise(100.0, Some(4)), "100");
        assert_eq!(format_copy_precise(-2.675, Some(1)), "-2.7");
    }

    #[test]
    fn copy_none_keeps_full_repr() {
        assert_eq!(
            format_copy_precise(1.0_f64 / 3.0, None),
            "0.3333333333333333"
        );
    }

    #[test]
    fn copy_precision_handles_non_finite() {
        assert_eq!(format_copy_precise(f64::INFINITY, Some(2)), "inf");
    }

    #[test]
    fn history_value_keeps_four_decimals_for_normal_values() {
        assert_eq!(format_history_value(42.5), "42.5000");
    }

    #[test]
    fn history_value_falls_back_to_full_repr_when_rounded_to_zero() {
        assert_eq!(format_history_value(0.000_012_34), "0.00001234");
    }

    #[test]
    fn history_value_falls_back_to_full_repr_for_negative_tiny_values() {
        assert_eq!(format_history_value(-0.000_01), "-0.00001");
    }

    #[test]
    fn history_value_keeps_zero_as_zero() {
        assert_eq!(format_history_value(0.0), "0.0000");
    }
}
