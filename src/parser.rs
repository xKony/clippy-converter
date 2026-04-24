use anyhow::{Result, anyhow};

/// Result of a successful string parse.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedInput {
    /// The numeric value extracted from the string.
    pub value: f64,
    /// The optional unit symbol or abbreviation extracted.
    pub unit: Option<String>,
}

/// Parses a string into a numeric value and an optional unit.
///
/// This function attempts to extract a number and a unit from the input string.
/// It supports various formats, including leading currency symbols, numbers
/// followed by units, and plain numbers. Whitespace is ignored between
/// the number and the unit.
///
/// # Errors
/// Returns an error if no number can be found in the input string.
///
/// # Examples
/// ```
/// use clippy_converter::parser::parse_input;
///
/// let result = parse_input("$50.5").unwrap();
/// assert_eq!(result.value, 50.5);
/// assert_eq!(result.unit, Some("USD".to_string()));
/// ```
pub fn parse_input(input: &str) -> Result<ParsedInput> {
    let input = input.trim();
    if input.is_empty() {
        return Err(anyhow!("Empty input string"));
    }

    // Common currency symbol mappings
    let symbols = [
        ('$', "USD"),
        ('€', "EUR"),
        ('£', "GBP"),
        ('¥', "JPY"),
        ('₹', "INR"),
        ('₪', "ILS"),
        ('₩', "KRW"),
        ('₽', "RUB"),
    ];

    // Check for leading currency symbol
    for (sym, unit) in symbols {
        if input.starts_with(sym) {
            let value_raw = input[sym.len_utf8()..].trim();
            let value_str = value_raw.replace(|c: char| c.is_whitespace(), "");
            let value: f64 = value_str
                .parse()
                .map_err(|_| anyhow!("Invalid number format after symbol: {value_raw}"))?;
            return Ok(ParsedInput {
                value,
                unit: Some(unit.to_string()),
            });
        }
    }

    // Try to find where the number ends and the unit starts
    let mut number_end = 0;
    let mut found_digit = false;
    let mut found_decimal = false;

    for (i, c) in input.char_indices() {
        if c.is_ascii_digit() {
            found_digit = true;
            number_end = i + 1;
        } else if c == '.' && !found_decimal {
            found_decimal = true;
            number_end = i + 1;
        } else if c.is_whitespace() {
            // Peek ahead to see if more digits or a decimal follow
            let remaining = &input[i + 1..];
            let mut is_part_of_number = false;
            for nc in remaining.chars() {
                if nc.is_ascii_digit() || (nc == '.' && !found_decimal) {
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
            // Reached potential unit start
            break;
        } else if c == '-' && !found_digit && !found_decimal {
            // Negative sign at start
            number_end = i + 1;
        } else {
            // Invalid character for number
            break;
        }
    }

    if !found_digit {
        return Err(anyhow!("No numeric value found in: {input}"));
    }

    let value_raw = &input[..number_end];
    let value_str = value_raw.replace(|c: char| c.is_whitespace(), "");
    let value: f64 = value_str
        .parse()
        .map_err(|_| anyhow!("Failed to parse numeric part: {value_raw}"))?;

    let unit_str = input[number_end..].trim();
    let unit = if unit_str.is_empty() {
        None
    } else {
        Some(unit_str.to_string())
    };

    Ok(ParsedInput { value, unit })
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
}
