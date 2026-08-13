//! A structural pre-pass over `manifest.json` that enforces the hostile-input limits
//! `serde_json` cannot express.
//!
//! `serde_json` is a good parser but its object model is a map: a document with the same
//! key twice deserialises silently, last one wins. `manifest-v1` §10.3 forbids that,
//! because a scanner and a validator that disagree about what a bundle claims is a
//! parser-differential bug. Depth, per-container element counts and the 4 KiB string cap
//! are equally invisible to a typed deserialiser once it has already allocated.
//!
//! This pass is a tokeniser, not a parser: it only has to get string boundaries right,
//! which is what makes `{` inside a string harmless. Everything grammatical is left to
//! `serde_json`, which runs afterwards on the same bytes. It is iterative — the container
//! stack lives on the heap — so nesting cannot overflow the machine stack.

use std::collections::BTreeSet;

use crate::error::{BundleError, BundleErrorKind, BundleResult};
use crate::limits::{MAX_ARRAY_ELEMENTS, MAX_DEPTH, MAX_OBJECT_KEYS, MAX_STRING_BYTES};

enum Frame {
    Object {
        keys: BTreeSet<String>,
        pending_key: Option<String>,
    },
    Array {
        elements: usize,
        non_empty: bool,
    },
}

fn err(kind: BundleErrorKind, detail: impl Into<String>) -> BundleError {
    BundleError::new(kind, detail)
}

/// [main-thread] Enforces `manifest-v1` §10.1/§10.3 on raw JSON text.
///
/// Checks, in one linear pass: nesting depth, duplicate keys within one object, key count
/// per object, element count per array, and the decoded length of every string.
///
/// # Errors
///
/// [`BundleErrorKind::DepthExceeded`], [`BundleErrorKind::DuplicateKey`],
/// [`BundleErrorKind::LimitExceeded`] or [`BundleErrorKind::Parse`] for an unterminated
/// or malformed string token.
pub fn prescan(input: &str) -> BundleResult<()> {
    let bytes = input.as_bytes();
    let mut stack: Vec<Frame> = Vec::new();
    let mut index = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'{' | b'[' => {
                if stack.len() >= MAX_DEPTH {
                    return Err(err(
                        BundleErrorKind::DepthExceeded,
                        format!("nesting deeper than {MAX_DEPTH}"),
                    ));
                }
                note_array_element(&mut stack)?;
                stack.push(if byte == b'{' {
                    Frame::Object {
                        keys: BTreeSet::new(),
                        pending_key: None,
                    }
                } else {
                    Frame::Array {
                        elements: 0,
                        non_empty: false,
                    }
                });
                index += 1;
            }
            b'}' | b']' => {
                if stack.pop().is_none() {
                    return Err(err(BundleErrorKind::Parse, "unbalanced closing bracket"));
                }
                index += 1;
            }
            b',' => {
                if let Some(Frame::Array { elements, .. }) = stack.last_mut() {
                    *elements += 1;
                    if *elements > MAX_ARRAY_ELEMENTS {
                        return Err(err(
                            BundleErrorKind::LimitExceeded,
                            format!("more than {MAX_ARRAY_ELEMENTS} elements in one array"),
                        ));
                    }
                }
                index += 1;
            }
            b'"' => {
                let (text, next) = scan_string(bytes, index)?;
                index = next;
                let is_key = matches!(stack.last(), Some(Frame::Object { .. }))
                    && next_significant(bytes, index) == Some(b':');
                if is_key {
                    if let Some(Frame::Object { pending_key, .. }) = stack.last_mut() {
                        *pending_key = Some(text);
                    }
                } else {
                    note_array_element(&mut stack)?;
                }
            }
            b':' => {
                if let Some(Frame::Object { keys, pending_key }) = stack.last_mut()
                    && let Some(key) = pending_key.take()
                {
                    if keys.len() >= MAX_OBJECT_KEYS {
                        return Err(err(
                            BundleErrorKind::LimitExceeded,
                            format!("more than {MAX_OBJECT_KEYS} keys in one object"),
                        ));
                    }
                    if !keys.insert(key.clone()) {
                        return Err(err(
                            BundleErrorKind::DuplicateKey,
                            format!("key `{key}` appears twice in one object"),
                        ));
                    }
                }
                index += 1;
            }
            b' ' | b'\t' | b'\n' | b'\r' => index += 1,
            _ => {
                // A literal or a number: count it as an array element and skip its run.
                note_array_element(&mut stack)?;
                index += 1;
                while index < bytes.len() && is_scalar_byte(bytes[index]) {
                    index += 1;
                }
            }
        }
    }

    if stack.is_empty() {
        Ok(())
    } else {
        Err(err(BundleErrorKind::Parse, "unterminated container"))
    }
}

fn is_scalar_byte(byte: u8) -> bool {
    !matches!(
        byte,
        b'{' | b'}' | b'[' | b']' | b',' | b':' | b'"' | b' ' | b'\t' | b'\n' | b'\r'
    )
}

fn note_array_element(stack: &mut [Frame]) -> BundleResult<()> {
    if let Some(Frame::Array {
        elements,
        non_empty,
    }) = stack.last_mut()
    {
        if !*non_empty {
            *non_empty = true;
            *elements += 1;
        }
        if *elements > MAX_ARRAY_ELEMENTS {
            return Err(err(
                BundleErrorKind::LimitExceeded,
                format!("more than {MAX_ARRAY_ELEMENTS} elements in one array"),
            ));
        }
    }
    Ok(())
}

fn next_significant(bytes: &[u8], from: usize) -> Option<u8> {
    let mut index = from;
    while index < bytes.len() {
        match bytes[index] {
            b' ' | b'\t' | b'\n' | b'\r' => index += 1,
            other => return Some(other),
        }
    }
    None
}

/// Reads the string token starting at `bytes[start] == b'"'`.
///
/// Returns the decoded text and the index just past the closing quote. Decoding matters
/// for two reasons: `"a"` and `"a"` are the same object key, and the 4 KiB cap is on
/// the value, not on its escaped spelling.
fn scan_string(bytes: &[u8], start: usize) -> BundleResult<(String, usize)> {
    let mut index = start + 1;
    let mut out = String::new();
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                return Ok((out, index + 1));
            }
            b'\\' => {
                index += 1;
                let escape = *bytes
                    .get(index)
                    .ok_or_else(|| err(BundleErrorKind::Parse, "truncated string escape"))?;
                index += 1;
                match escape {
                    b'u' => {
                        let (ch, next) = scan_unicode_escape(bytes, index)?;
                        index = next;
                        out.push(ch);
                    }
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'"' | b'\\' | b'/' => out.push(char::from(escape)),
                    other => {
                        return Err(err(
                            BundleErrorKind::Parse,
                            format!("unknown string escape `\\{}`", char::from(other)),
                        ));
                    }
                }
            }
            _ => {
                // `input` is valid UTF-8, so the byte at `index` starts a character.
                let rest = &bytes[index..];
                let text = core::str::from_utf8(rest)
                    .map_err(|_| err(BundleErrorKind::Encoding, "invalid UTF-8 in string"))?;
                let ch = text
                    .chars()
                    .next()
                    .ok_or_else(|| err(BundleErrorKind::Parse, "unterminated string"))?;
                out.push(ch);
                index += ch.len_utf8();
            }
        }
        if out.len() > MAX_STRING_BYTES {
            return Err(err(
                BundleErrorKind::LimitExceeded,
                format!("string value longer than {MAX_STRING_BYTES} bytes"),
            ));
        }
    }
    Err(err(BundleErrorKind::Parse, "unterminated string"))
}

fn scan_unicode_escape(bytes: &[u8], start: usize) -> BundleResult<(char, usize)> {
    let unit = read_hex4(bytes, start)?;
    let mut index = start + 4;
    if (0xD800..0xDC00).contains(&unit) {
        // High surrogate: a low surrogate must follow for the pair to be a character.
        if bytes.get(index) == Some(&b'\\') && bytes.get(index + 1) == Some(&b'u') {
            let low = read_hex4(bytes, index + 2)?;
            if (0xDC00..0xE000).contains(&low) {
                index += 6;
                let combined =
                    0x1_0000 + ((u32::from(unit) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
                let ch = char::from_u32(combined)
                    .ok_or_else(|| err(BundleErrorKind::Parse, "invalid surrogate pair"))?;
                return Ok((ch, index));
            }
        }
        return Err(err(BundleErrorKind::Parse, "unpaired surrogate"));
    }
    if (0xDC00..0xE000).contains(&unit) {
        return Err(err(BundleErrorKind::Parse, "unpaired low surrogate"));
    }
    let ch = char::from_u32(u32::from(unit))
        .ok_or_else(|| err(BundleErrorKind::Parse, "invalid escape"))?;
    Ok((ch, index))
}

fn read_hex4(bytes: &[u8], start: usize) -> BundleResult<u16> {
    let slice = bytes
        .get(start..start + 4)
        .ok_or_else(|| err(BundleErrorKind::Parse, "truncated `\\u` escape"))?;
    let mut value: u16 = 0;
    for byte in slice {
        let digit = char::from(*byte)
            .to_digit(16)
            .ok_or_else(|| err(BundleErrorKind::Parse, "non-hex digit in `\\u` escape"))?;
        // Four hex digits never overflow a `u16`.
        value = value * 16 + u16::try_from(digit).unwrap_or(0);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_well_formed_documents() {
        for good in [
            "{}",
            "[]",
            r#"{"a":1,"b":[1,2,3],"c":{"d":null}}"#,
            r#"{"a":"quote \" brace { bracket [ colon : comma ,"}"#,
            r#"{"a":"é😀"}"#,
            r#"  {  "a" : true }  "#,
            r#"[[[[[[[[1]]]]]]]]"#,
        ] {
            prescan(good).unwrap_or_else(|err| panic!("{good}: {err}"));
        }
    }

    #[test]
    fn rejects_duplicate_keys() {
        let err = prescan(r#"{"id":"a","id":"b"}"#).expect_err("duplicate");
        assert_eq!(err.kind(), &BundleErrorKind::DuplicateKey);
        assert_eq!(err.code(), "DAUX-M019");

        // Escaped spellings of the same key are the same key.
        let err = prescan(r#"{"id":1,"id":2}"#).expect_err("escaped duplicate");
        assert_eq!(err.kind(), &BundleErrorKind::DuplicateKey);

        // The same key in *different* objects is fine.
        prescan(r#"{"a":{"id":1},"b":{"id":2}}"#).expect("distinct objects");
    }

    #[test]
    fn rejects_excessive_depth() {
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        let err = prescan(&deep).expect_err("too deep");
        assert_eq!(err.kind(), &BundleErrorKind::DepthExceeded);
        assert_eq!(err.code(), "DAUX-M018");

        let ok = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        prescan(&ok).expect("exactly at the limit");
    }

    #[test]
    fn rejects_pathological_nesting_without_overflowing_the_stack() {
        // 200_000 levels would blow a recursive-descent parser's stack; this one is
        // iterative, so it returns a plain error.
        let deep = "[".repeat(200_000);
        let err = prescan(&deep).expect_err("too deep");
        assert_eq!(err.kind(), &BundleErrorKind::DepthExceeded);
    }

    #[test]
    fn rejects_over_long_strings() {
        let long = format!(r#"{{"a":"{}"}}"#, "x".repeat(MAX_STRING_BYTES + 1));
        let err = prescan(&long).expect_err("too long");
        assert_eq!(err.kind(), &BundleErrorKind::LimitExceeded);

        let escaped = format!(r#"{{"a":"{}"}}"#, "\\n".repeat(MAX_STRING_BYTES + 1));
        assert!(prescan(&escaped).is_err());

        // A string whose escaped spelling is long but whose value is short is accepted.
        let short_value = format!(r#"{{"a":"{}"}}"#, "\\u0041".repeat(64));
        prescan(&short_value).expect("64 characters");
    }

    #[test]
    fn counts_array_elements() {
        let many = format!("[{}]", vec!["1"; MAX_ARRAY_ELEMENTS + 1].join(","));
        let err = prescan(&many).expect_err("too many");
        assert_eq!(err.kind(), &BundleErrorKind::LimitExceeded);

        let ok = format!("[{}]", ["1"; 8].join(","));
        prescan(&ok).expect("small array");
        prescan("[]").expect("empty array");
    }

    #[test]
    fn counts_object_keys() {
        let mut document = String::from("{");
        for index in 0..=MAX_OBJECT_KEYS {
            if index > 0 {
                document.push(',');
            }
            document.push_str(&format!("\"k{index}\":1"));
        }
        document.push('}');
        let err = prescan(&document).expect_err("too many keys");
        assert_eq!(err.kind(), &BundleErrorKind::LimitExceeded);
    }

    #[test]
    fn rejects_malformed_strings() {
        for bad in [
            r#"{"a":"unterminated"#,
            r#"{"a":"bad \q escape"}"#,
            r#"{"a":"\u12"}"#,
            r#"{"a":"\ud800"}"#,
            r#"{"a":"\udc00"}"#,
            r#"{"a":"\"#,
        ] {
            let err = prescan(bad).expect_err(bad);
            assert!(
                matches!(
                    err.kind(),
                    BundleErrorKind::Parse | BundleErrorKind::LimitExceeded
                ),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn rejects_unbalanced_containers() {
        assert!(prescan("{").is_err());
        assert!(prescan("}").is_err());
        assert!(prescan("[1,2").is_err());
        assert!(prescan(r#"{"a":1}]"#).is_err());
    }

    #[test]
    fn tolerates_empty_and_scalar_documents() {
        // Structural validity is `serde_json`'s job; this pass only enforces the limits.
        prescan("").expect("empty input has no violation to report");
        prescan("null").expect("scalar");
        prescan("123456").expect("number");
    }
}
