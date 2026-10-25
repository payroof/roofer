//! Exact decimal amounts and atomic units as decimal strings (up to uint256), no rounding.

use anyhow::{Result, bail};

const UINT256_MAX: &str =
    "115792089237316195423570985084696718986263464705712411950427093016397619735";

fn trim_zeros(digits: &str) -> &str {
    let t = digits.trim_start_matches('0');
    if t.is_empty() { "0" } else { t }
}

/// Compares two unsigned decimal integers.
pub fn cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (trim_zeros(a), trim_zeros(b));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Exact decimal -> atomic units; rejects excess precision, zero and values above uint256.
pub fn parse_units(amount: &str, decimals: u32) -> Result<String> {
    let valid = !amount.is_empty()
        && amount.chars().all(|c| c.is_ascii_digit() || c == '.')
        && amount.matches('.').count() <= 1
        && !amount.ends_with('.');
    if !valid {
        bail!("invalid amount '{amount}' (use a plain decimal like 1, 0.5, 250.75)");
    }
    let (whole, frac) = amount.split_once('.').unwrap_or((amount, ""));
    let frac = frac.trim_end_matches('0'); // trailing zeros do not count as precision
    if frac.len() > decimals as usize {
        bail!("amount '{amount}' has more than {decimals} decimal places");
    }
    let mut digits = format!("{whole}{frac}");
    digits.push_str(&"0".repeat(decimals as usize - frac.len()));
    let units = trim_zeros(&digits).to_string();
    if units == "0" {
        bail!("amount must be greater than zero");
    }
    if cmp(&units, UINT256_MAX).is_gt() {
        bail!("amount '{amount}' out of range");
    }
    Ok(units)
}

/// Atomic units -> exact decimal, without trailing zeros.
pub fn format_units(units: &str, decimals: u32) -> String {
    let digits = trim_zeros(units);
    let d = decimals as usize;
    let padded = if digits.len() <= d {
        format!("{}{digits}", "0".repeat(d + 1 - digits.len()))
    } else {
        digits.to_string()
    };
    let (whole, frac) = padded.split_at(padded.len() - d);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{frac}")
    }
}

/// A `0x` hex quantity -> decimal string.
pub fn hex_to_decimal(hex: &str) -> Option<String> {
    let hex = hex.strip_prefix("0x")?;
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut digits: Vec<u8> = vec![0]; // little-endian base-10
    for c in hex.chars() {
        let mut carry = c.to_digit(16)?;
        for d in digits.iter_mut() {
            let v = *d as u32 * 16 + carry;
            *d = (v % 10) as u8;
            carry = v / 10;
        }
        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
    }
    Some(
        trim_zeros(
            &digits
                .iter()
                .rev()
                .map(|d| char::from(b'0' + d))
                .collect::<String>(),
        )
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_amounts() {
        assert_eq!(parse_units("1", 6).unwrap(), "1000000");
        assert_eq!(parse_units("0.5", 18).unwrap(), "500000000000000000");
        assert_eq!(parse_units(".25", 2).unwrap(), "25");
        assert_eq!(parse_units("250.7500", 2).unwrap(), "25075");
        assert_eq!(
            parse_units("1000000000", 36).unwrap(),
            format!("1{}", "0".repeat(45))
        );
    }

    #[test]
    fn rejects_bad_amounts() {
        for bad in ["", "1.", "-1", "1e5", "1.2.3", "abc", "0", "0.000"] {
            assert!(parse_units(bad, 6).is_err(), "{bad}");
        }
        assert!(parse_units("0.1234567", 6).is_err());
        assert!(parse_units("1", 0).is_ok());
        assert!(parse_units(UINT256_MAX, 0).is_ok());
        assert!(parse_units(&format!("{UINT256_MAX}0"), 0).is_err());
    }

    #[test]
    fn formats_units() {
        assert_eq!(format_units("1500000", 6), "1.5");
        assert_eq!(format_units("0", 18), "0");
        assert_eq!(format_units("5", 3), "0.005");
        assert_eq!(format_units("10000000000000000000", 18), "10");
        assert_eq!(format_units("42", 0), "42");
    }

    #[test]
    fn compares_and_converts() {
        assert!(cmp("99", "100").is_lt());
        assert!(cmp("0100", "100").is_eq());
        assert_eq!(hex_to_decimal("0x7a69").as_deref(), Some("31337"));
        assert_eq!(hex_to_decimal("0x0").as_deref(), Some("0"));
        assert_eq!(
            hex_to_decimal("0xde0b6b3a7640000").as_deref(),
            Some("1000000000000000000")
        );
        assert!(hex_to_decimal("12").is_none());
    }
}
