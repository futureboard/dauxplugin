//! Formatting and parsing of plain parameter values.
//!
//! Hosts and editors round-trip parameter values through text constantly: a generic UI
//! shows `"-6.0 dB"`, the user types `"-6"` or `" -6.0 dB "` or `"6dB"` back, and the
//! plug-in has to accept all of it. These helpers implement that leniency once so every
//! parameter type behaves the same way.
//!
//! Formatting writes into a caller-owned [`String`] and allocates nothing beyond that
//! buffer's own growth — give it a `String::with_capacity(32)` once and it never
//! allocates again. Parsing allocates nothing at all.

use core::fmt::Write as _;

/// Largest number of fraction digits [`format_value`] honours.
///
/// Beyond this the extra digits are noise from binary rounding, and the fixed cap keeps
/// the formatted length bounded for the `DauxText` buffers of `abi-v1` §2.
pub const MAX_DECIMALS: u8 = 12;

/// `[main-thread]` Writes `value` with `decimals` fraction digits and an optional unit.
///
/// The previous contents of `out` are replaced. Non-finite values are written as
/// `"inf"`, `"-inf"` and `"nan"` — `-inf dB` is a perfectly ordinary thing for a gain
/// control to display, and it parses back through [`parse_value`].
///
/// ```
/// # use daux_parameter::text::format_value;
/// let mut s = String::with_capacity(32);
/// format_value(-6.0, 1, "dB", &mut s);
/// assert_eq!(s, "-6.0 dB");
/// format_value(0.5, 3, "", &mut s);
/// assert_eq!(s, "0.500");
/// ```
pub fn format_value(value: f64, decimals: u8, unit: &str, out: &mut String) {
    out.clear();
    if value.is_nan() {
        out.push_str("nan");
    } else if value.is_infinite() {
        out.push_str(if value < 0.0 { "-inf" } else { "inf" });
    } else {
        let places = decimals.min(MAX_DECIMALS) as usize;
        // Writing into a `String` is infallible; the `Result` only exists because
        // `fmt::Write` is generic over sinks that can fail.
        let _ = write!(out, "{value:.places$}");
    }
    if !unit.is_empty() {
        out.push(' ');
        out.push_str(unit);
    }
}

/// `[main-thread]` Parses a plain value out of user-entered text.
///
/// Accepted, in order of attempt:
///
/// * surrounding whitespace, e.g. `"  -6.0 dB "`;
/// * a leading sign, e.g. `"+3"`;
/// * a unit suffix, attached or not, e.g. `"6dB"`, `"440 Hz"`, `"50%"`;
/// * scientific notation, e.g. `"1e3"`, `"-2.5E-2"`;
/// * infinities written as `"inf"`, `"-inf"`, `"+Infinity"` or `"-∞"`.
///
/// Returns `None` when there is no number to find (`""`, `"abc"`, `"dB"`) or when the
/// text parses to `NaN`. The decimal separator is `.`; a `,` ends the number, so
/// `"1,5"` parses as `1`.
///
/// ```
/// # use daux_parameter::text::parse_value;
/// assert_eq!(parse_value("  -6.0 dB "), Some(-6.0));
/// assert_eq!(parse_value("6dB"), Some(6.0));
/// assert_eq!(parse_value("1e3"), Some(1000.0));
/// assert_eq!(parse_value("-inf"), Some(f64::NEG_INFINITY));
/// assert_eq!(parse_value("nonsense"), None);
/// ```
#[must_use]
pub fn parse_value(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let bytes = trimmed.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    if matches!(bytes[i], b'+' | b'-') {
        i += 1;
    }

    let mut digits = 0;
    while i < len && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < len && bytes[i] == b'.' {
        i += 1;
        while i < len && bytes[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        // No mantissa: the only remaining possibility is a written-out infinity.
        return parse_infinite(trimmed);
    }

    // An exponent only counts if it actually has digits, so that the `e` of a unit
    // like `"3 e"` does not swallow the number.
    let mut end = i;
    if i < len && matches!(bytes[i], b'e' | b'E') {
        let mut j = i + 1;
        if j < len && matches!(bytes[j], b'+' | b'-') {
            j += 1;
        }
        let exponent_start = j;
        while j < len && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > exponent_start {
            end = j;
        }
    }

    // `end` only ever lands after ASCII bytes, so this slice is on a char boundary.
    let value: f64 = trimmed[..end].parse().ok()?;
    if value.is_nan() { None } else { Some(value) }
}

/// Recognises `inf`, `infinity` and `∞` with an optional sign and inner whitespace.
fn parse_infinite(trimmed: &str) -> Option<f64> {
    let (sign, rest) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1.0_f64, rest),
        None => (1.0_f64, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let rest = rest.trim_start();
    let is_inf = rest.starts_with('∞')
        || rest
            .as_bytes()
            .get(..3)
            .is_some_and(|head| head.eq_ignore_ascii_case(b"inf"));
    is_inf.then_some(sign * f64::INFINITY)
}

/// `[main-thread]` Parses the text of a two-state control.
///
/// Accepts `on`/`off`, `true`/`false`, `yes`/`no`, `enabled`/`disabled` in any case,
/// and falls back to a number (`>= 0.5` is on), so `"1"`, `"0"` and `"0.75"` work too.
/// Returns `None` for anything else, which lets a caller keep the current value instead
/// of silently toggling it.
#[must_use]
pub fn parse_bool(text: &str) -> Option<bool> {
    let t = text.trim();
    const TRUE_WORDS: [&str; 5] = ["on", "true", "yes", "enabled", "y"];
    const FALSE_WORDS: [&str; 5] = ["off", "false", "no", "disabled", "n"];

    if TRUE_WORDS.iter().any(|w| t.eq_ignore_ascii_case(w)) {
        return Some(true);
    }
    if FALSE_WORDS.iter().any(|w| t.eq_ignore_ascii_case(w)) {
        return Some(false);
    }
    parse_value(t).map(|v| v >= 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formatted(value: f64, decimals: u8, unit: &str) -> String {
        let mut s = String::new();
        format_value(value, decimals, unit, &mut s);
        s
    }

    #[test]
    fn formats_with_decimals_and_unit() {
        assert_eq!(formatted(-6.0, 1, "dB"), "-6.0 dB");
        assert_eq!(formatted(-6.0, 0, "dB"), "-6 dB");
        assert_eq!(formatted(0.5, 3, ""), "0.500");
        assert_eq!(formatted(440.0, 2, "Hz"), "440.00 Hz");
        assert_eq!(formatted(1.0 / 3.0, 4, "%"), "0.3333 %");
    }

    #[test]
    fn formatting_replaces_previous_contents() {
        let mut s = String::from("stale text that is longer");
        format_value(1.0, 0, "", &mut s);
        assert_eq!(s, "1");
    }

    #[test]
    fn decimals_are_capped() {
        let s = formatted(1.0, 200, "");
        assert_eq!(s.len(), 2 + MAX_DECIMALS as usize);
        assert!(s.starts_with("1."));
    }

    #[test]
    fn non_finite_values_are_readable_and_reparsable() {
        assert_eq!(formatted(f64::NEG_INFINITY, 2, "dB"), "-inf dB");
        assert_eq!(formatted(f64::INFINITY, 2, ""), "inf");
        assert_eq!(formatted(f64::NAN, 2, ""), "nan");
        assert_eq!(parse_value("-inf dB"), Some(f64::NEG_INFINITY));
        assert_eq!(parse_value("inf"), Some(f64::INFINITY));
        assert_eq!(parse_value("nan"), None);
    }

    #[test]
    fn parses_messy_user_input() {
        assert_eq!(parse_value("  -6.0 dB "), Some(-6.0));
        assert_eq!(parse_value("6dB"), Some(6.0));
        assert_eq!(parse_value("+3"), Some(3.0));
        assert_eq!(parse_value("3."), Some(3.0));
        assert_eq!(parse_value(".5"), Some(0.5));
        assert_eq!(parse_value("-.5"), Some(-0.5));
        assert_eq!(parse_value("50%"), Some(50.0));
        assert_eq!(parse_value("440 Hz"), Some(440.0));
        assert_eq!(parse_value("\t 12\u{a0}"), Some(12.0));
        assert_eq!(parse_value("0"), Some(0.0));
    }

    #[test]
    fn parses_scientific_notation_without_eating_units() {
        assert_eq!(parse_value("1e3"), Some(1000.0));
        assert_eq!(parse_value("1E3"), Some(1000.0));
        assert_eq!(parse_value("-2.5E-2"), Some(-0.025));
        assert_eq!(parse_value("1e+3 Hz"), Some(1000.0));
        // A unit that merely starts with `e` must not be mistaken for an exponent.
        assert_eq!(parse_value("3 e"), Some(3.0));
        assert_eq!(parse_value("3e"), Some(3.0));
        assert_eq!(parse_value("3eV"), Some(3.0));
        assert_eq!(parse_value("3e-"), Some(3.0));
    }

    #[test]
    fn rejects_input_without_a_number() {
        assert_eq!(parse_value(""), None);
        assert_eq!(parse_value("   "), None);
        assert_eq!(parse_value("dB"), None);
        assert_eq!(parse_value("abc"), None);
        assert_eq!(parse_value("-"), None);
        assert_eq!(parse_value("+"), None);
        assert_eq!(parse_value("."), None);
        assert_eq!(parse_value("-."), None);
        assert_eq!(parse_value("e5"), None);
    }

    #[test]
    fn handles_non_ascii_without_panicking() {
        // Slicing must never split a multi-byte character.
        assert_eq!(parse_value("−6"), None); // U+2212 MINUS SIGN is not ASCII '-'
        assert_eq!(parse_value("6 °C"), Some(6.0));
        assert_eq!(parse_value("∞"), Some(f64::INFINITY));
        assert_eq!(parse_value("-∞"), Some(f64::NEG_INFINITY));
        assert_eq!(parse_value("π"), None);
        assert_eq!(parse_value("i"), None);
        assert_eq!(parse_value("in"), None);
    }

    #[test]
    fn comma_terminates_the_number() {
        assert_eq!(parse_value("1,5"), Some(1.0));
    }

    #[test]
    fn format_parse_round_trips() {
        for value in [0.0, -6.0, 12.5, -0.125, 1000.0, -48.75, 0.001] {
            let text = formatted(value, 3, "dB");
            let parsed = parse_value(&text).expect("formatted output must parse");
            assert!(
                (parsed - value).abs() < 1e-3,
                "{value} -> {text} -> {parsed}"
            );
        }
    }

    #[test]
    fn parses_boolean_words_and_numbers() {
        for on in [
            "on", "ON", "On", "true", "TRUE", "yes", "enabled", "y", "1", "0.75",
        ] {
            assert_eq!(parse_bool(on), Some(true), "{on}");
        }
        for off in [
            "off", "OFF", "false", "no", "disabled", "n", "0", "0.25", "-1",
        ] {
            assert_eq!(parse_bool(off), Some(false), "{off}");
        }
        assert_eq!(parse_bool("  on  "), Some(true));
        assert_eq!(parse_bool("maybe"), None);
        assert_eq!(parse_bool(""), None);
    }
}
