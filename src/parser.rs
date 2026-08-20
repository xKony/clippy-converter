use anyhow::{Result, anyhow};

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

/// Result of a successful string parse.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedInput {
    /// The numeric value extracted from the string.
    pub value: f64,
    /// The optional source unit symbol or abbreviation extracted.
    pub unit: Option<String>,
    /// Optional explicit target from phrases like `100 USD to PLN`.
    pub target: Option<String>,
}

/// Parses a string into a numeric value and an optional unit.
///
/// Supports leading/trailing currency symbols, grouped digits (`1,234.56` /
/// `1.234,56`), and `to`/`in` target clauses.
///
/// # Errors
/// Returns an error if no number can be found in the input string.
pub fn parse_input(input: &str) -> Result<ParsedInput> {
    let input = input.trim();
    if input.is_empty() {
        return Err(anyhow!("Empty input string"));
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

    let (number_end, found_digit) = scan_number_end(core_input);
    if !found_digit {
        return Err(anyhow!("No numeric value found in: {input}"));
    }

    let value_raw = &core_input[..number_end];
    let value_str = normalize_numeric(value_raw);
    let value: f64 = value_str
        .parse()
        .map_err(|_| anyhow!("Failed to parse numeric part: {value_raw}"))?;

    let mut unit_str = core_input[number_end..].trim();
    if symbol_unit.is_none()
        && let Some((rest, unit)) = strip_trailing_currency(unit_str)
    {
        unit_str = rest;
        symbol_unit = Some(unit);
    }

    let (source, target) = split_source_target(unit_str);
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

    Ok(ParsedInput {
        value,
        unit,
        target,
    })
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

/// Turns grouped/locale number text into something `f64::parse` accepts.
fn normalize_numeric(raw: &str) -> String {
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains('e') || compact.contains('E') {
        return compact.replace(',', "");
    }

    let last_comma = compact.rfind(',');
    let last_dot = compact.rfind('.');
    match (last_comma, last_dot) {
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
    }
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
}
