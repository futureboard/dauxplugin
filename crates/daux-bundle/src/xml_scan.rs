//! A structural pre-pass over an XML `Info.plist`, mirroring [`crate::json_scan`].
//!
//! The `plist` crate builds its value tree iteratively, so nesting cannot overflow the
//! machine stack, and its readers return errors rather than panicking. What it does *not*
//! do is reject a dictionary that carries the same `<key>` twice, refuse a document with a
//! DTD internal subset, or bound container sizes — all of which `manifest-v1` §10.1/§10.3
//! require of a conforming reader. This pass runs first and answers exactly those
//! questions.
//!
//! It is a tag scanner, not an XML parser. It has to get four things right — quoted
//! attribute values, comments, CDATA sections and processing instructions — because those
//! are the constructs that can hide a `<` or a `>`. Everything else is left to the real
//! parser that runs afterwards on the same bytes.

use std::collections::BTreeSet;

use crate::error::{BundleError, BundleErrorKind, BundleResult};
use crate::limits::{MAX_ARRAY_ELEMENTS, MAX_DEPTH, MAX_OBJECT_KEYS, MAX_STRING_BYTES};

enum Frame {
    Dict { keys: BTreeSet<String> },
    Array { elements: usize },
}

fn err(kind: BundleErrorKind, detail: impl Into<String>) -> BundleError {
    BundleError::new(kind, detail)
}

/// [main-thread] Enforces `manifest-v1` §10.1/§10.3 on raw XML property-list text.
///
/// # Errors
///
/// * [`BundleErrorKind::Parse`] — a DTD with an internal subset (the entity-expansion
///   vector), an unterminated comment/CDATA/tag, or a `</dict>` that closes an `<array>`.
/// * [`BundleErrorKind::DepthExceeded`] — `<dict>`/`<array>` nesting past
///   [`MAX_DEPTH`](crate::limits::MAX_DEPTH).
/// * [`BundleErrorKind::DuplicateKey`] — the same `<key>` twice in one `<dict>`.
/// * [`BundleErrorKind::LimitExceeded`] — too many keys, elements, or an over-long key.
pub fn prescan(input: &str) -> BundleResult<()> {
    let bytes = input.as_bytes();
    let mut stack: Vec<Frame> = Vec::new();
    let mut index = 0usize;

    while index < bytes.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        if input[index..].starts_with("<!--") {
            index = find(input, index + 4, "-->")
                .ok_or_else(|| err(BundleErrorKind::Parse, "unterminated comment"))?
                + 3;
            continue;
        }
        if input[index..].starts_with("<![CDATA[") {
            index = find(input, index + 9, "]]>")
                .ok_or_else(|| err(BundleErrorKind::Parse, "unterminated CDATA section"))?
                + 3;
            continue;
        }
        if input[index..].starts_with("<?") {
            index = find(input, index + 2, "?>")
                .ok_or_else(|| {
                    err(BundleErrorKind::Parse, "unterminated processing instruction")
                })?
                + 2;
            continue;
        }
        if input[index..].starts_with("<!") {
            let end = find(input, index + 2, ">")
                .ok_or_else(|| err(BundleErrorKind::Parse, "unterminated declaration"))?;
            if input[index..end].contains('[') {
                // A DTD internal subset is where "billion laughs" lives. The XML reader
                // this crate uses does not expand entities, and neither does this one:
                // the document is refused instead (`manifest-v1` §10.3).
                return Err(err(
                    BundleErrorKind::Parse,
                    "DTD internal subset is not accepted",
                ));
            }
            index = end + 1;
            continue;
        }

        let (name, closing, self_closing, next) = scan_tag(input, index)?;
        index = next;

        if closing {
            match (name.as_str(), stack.pop()) {
                ("dict", Some(Frame::Dict { .. })) | ("array", Some(Frame::Array { .. })) => {}
                ("dict" | "array", _) => {
                    return Err(err(BundleErrorKind::Parse, format!("`</{name}>` is unbalanced")));
                }
                (_, Some(frame)) => stack.push(frame),
                (_, None) => {}
            }
            continue;
        }

        // Any element start that is a direct child of an array is one element of it.
        if let Some(Frame::Array { elements }) = stack.last_mut() {
            *elements += 1;
            if *elements > MAX_ARRAY_ELEMENTS {
                return Err(err(
                    BundleErrorKind::LimitExceeded,
                    format!("more than {MAX_ARRAY_ELEMENTS} elements in one array"),
                ));
            }
        }

        match name.as_str() {
            "dict" | "array" if !self_closing => {
                if stack.len() >= MAX_DEPTH {
                    return Err(err(
                        BundleErrorKind::DepthExceeded,
                        format!("nesting deeper than {MAX_DEPTH}"),
                    ));
                }
                stack.push(if name == "dict" {
                    Frame::Dict {
                        keys: BTreeSet::new(),
                    }
                } else {
                    Frame::Array { elements: 0 }
                });
            }
            "key" if !self_closing => {
                let (text, next) = scan_text(input, index, "</key>")?;
                index = next;
                let Some(Frame::Dict { keys }) = stack.last_mut() else {
                    return Err(err(BundleErrorKind::Parse, "`<key>` outside a `<dict>`"));
                };
                if keys.len() >= MAX_OBJECT_KEYS {
                    return Err(err(
                        BundleErrorKind::LimitExceeded,
                        format!("more than {MAX_OBJECT_KEYS} keys in one dictionary"),
                    ));
                }
                if !keys.insert(text.clone()) {
                    return Err(err(
                        BundleErrorKind::DuplicateKey,
                        format!("key `{text}` appears twice in one dictionary"),
                    ));
                }
            }
            _ => {}
        }
    }

    if stack.is_empty() {
        Ok(())
    } else {
        Err(err(BundleErrorKind::Parse, "unterminated `<dict>` or `<array>`"))
    }
}

fn find(input: &str, from: usize, needle: &str) -> Option<usize> {
    input.get(from..).and_then(|rest| rest.find(needle)).map(|at| from + at)
}

/// Reads the tag starting at `input[start] == '<'`.
///
/// Returns `(name, is_closing, is_self_closing, index just past '>')`. Attribute values
/// are skipped with their quoting respected, so `<plist version="a>b">` does not end
/// early.
fn scan_tag(input: &str, start: usize) -> BundleResult<(String, bool, bool, usize)> {
    let bytes = input.as_bytes();
    let mut index = start + 1;
    let closing = bytes.get(index) == Some(&b'/');
    if closing {
        index += 1;
    }

    let name_start = index;
    while index < bytes.len()
        && !matches!(bytes[index], b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
    {
        index += 1;
    }
    let name = input
        .get(name_start..index)
        .ok_or_else(|| err(BundleErrorKind::Parse, "malformed tag"))?
        .to_ascii_lowercase();
    if name.is_empty() {
        return Err(err(BundleErrorKind::Parse, "empty tag name"));
    }

    let mut quote: Option<u8> = None;
    while index < bytes.len() {
        let byte = bytes[index];
        match (quote, byte) {
            (Some(open), _) if byte == open => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(byte),
            (None, b'>') => {
                let self_closing = index > start && bytes[index - 1] == b'/';
                return Ok((name, closing, self_closing, index + 1));
            }
            (None, _) => {}
        }
        index += 1;
    }
    Err(err(BundleErrorKind::Parse, "unterminated tag"))
}

/// Reads element text from `start` up to `terminator`, decoding the predefined entities.
fn scan_text(input: &str, start: usize, terminator: &str) -> BundleResult<(String, usize)> {
    let end = find(input, start, terminator)
        .ok_or_else(|| err(BundleErrorKind::Parse, format!("missing `{terminator}`")))?;
    let raw = input
        .get(start..end)
        .ok_or_else(|| err(BundleErrorKind::Parse, "malformed element text"))?;
    if raw.len() > MAX_STRING_BYTES {
        return Err(err(
            BundleErrorKind::LimitExceeded,
            format!("element text longer than {MAX_STRING_BYTES} bytes"),
        ));
    }
    Ok((decode_entities(raw), end + terminator.len()))
}

/// Decodes the five predefined XML entities and numeric character references.
///
/// An unrecognised `&…;` run is left verbatim: this pass compares keys for equality and
/// must not invent a value the real parser would not produce.
fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_owned();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..semi];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            numeric if numeric.starts_with('#') => {
                let (digits, radix) = numeric
                    .strip_prefix("#x")
                    .or_else(|| numeric.strip_prefix("#X"))
                    .map_or((&numeric[1..], 10), |hex| (hex, 16));
                match u32::from_str_radix(digits, radix).ok().and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => out.push_str(&tail[..=semi]),
                }
            }
            _ => out.push_str(&tail[..=semi]),
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
        <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
        \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n";

    fn plist(body: &str) -> String {
        format!("{HEADER}{body}\n</plist>\n")
    }

    #[test]
    fn accepts_a_well_formed_document() {
        let document = plist(
            "<dict><key>CFBundleIdentifier</key><string>a.b</string>\
             <key>DAUxTargets</key><array><string>macos-arm64</string></array>\
             <key>DAUxGraphics</key><dict><key>enabled</key><true/></dict></dict>",
        );
        prescan(&document).expect("valid");
    }

    #[test]
    fn rejects_duplicate_keys_in_one_dict() {
        let document = plist("<dict><key>a</key><string>1</string><key>a</key><string>2</string></dict>");
        let err = prescan(&document).expect_err("duplicate");
        assert_eq!(err.kind(), &BundleErrorKind::DuplicateKey);

        // The same key in two sibling dictionaries is legal.
        let document = plist(
            "<array><dict><key>a</key><string>1</string></dict>\
             <dict><key>a</key><string>2</string></dict></array>",
        );
        prescan(&document).expect("sibling dicts");
    }

    #[test]
    fn entity_encoded_keys_collide_with_their_plain_spelling() {
        let document = plist("<dict><key>a&#98;</key><string>1</string><key>ab</key><string>2</string></dict>");
        let err = prescan(&document).expect_err("duplicate after decoding");
        assert_eq!(err.kind(), &BundleErrorKind::DuplicateKey);
    }

    #[test]
    fn rejects_dtd_internal_subset() {
        let document = "<!DOCTYPE plist [<!ENTITY lol \"haha\">]><plist><dict/></plist>";
        let err = prescan(document).expect_err("internal subset");
        assert_eq!(err.kind(), &BundleErrorKind::Parse);
    }

    #[test]
    fn rejects_excessive_depth() {
        let body = format!("{}{}", "<dict><key>k</key>".repeat(MAX_DEPTH + 1), "</dict>".repeat(MAX_DEPTH + 1));
        let err = prescan(&plist(&body)).expect_err("too deep");
        assert_eq!(err.kind(), &BundleErrorKind::DepthExceeded);
    }

    #[test]
    fn counts_array_elements() {
        let body = format!("<array>{}</array>", "<string>x</string>".repeat(MAX_ARRAY_ELEMENTS + 1));
        let err = prescan(&plist(&body)).expect_err("too many");
        assert_eq!(err.kind(), &BundleErrorKind::LimitExceeded);
    }

    #[test]
    fn counts_dictionary_keys() {
        let mut body = String::from("<dict>");
        for index in 0..=MAX_OBJECT_KEYS {
            body.push_str(&format!("<key>k{index}</key><string>v</string>"));
        }
        body.push_str("</dict>");
        let err = prescan(&plist(&body)).expect_err("too many keys");
        assert_eq!(err.kind(), &BundleErrorKind::LimitExceeded);
    }

    #[test]
    fn skips_comments_cdata_and_attributes() {
        let document = plist(
            "<!-- <dict><key>ignored</key> --><dict><key>a</key>\
             <string><![CDATA[<dict><key>also ignored</key>]]></string></dict>",
        );
        prescan(&document).expect("comments and CDATA are inert");

        let document = plist("<dict attr=\"a>b\"><key>a</key><string>1</string></dict>");
        prescan(&document).expect("quoted `>` in an attribute");
    }

    #[test]
    fn rejects_unbalanced_and_truncated_documents() {
        for bad in [
            plist("<dict><key>a</key><string>1</string></array>"),
            plist("<!-- unterminated"),
            plist("<![CDATA[unterminated"),
            plist("<dict"),
            plist("<key>orphan</key>"),
            plist("<dict><key>truncated"),
        ] {
            let err = prescan(&bad).expect_err(&bad);
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
    fn self_closing_containers_do_not_open_a_frame() {
        prescan(&plist("<dict/>")).expect("self-closing dict");
        prescan(&plist("<array/>")).expect("self-closing array");
        prescan(&plist("<dict><key>a</key><array/></dict>")).expect("nested self-closing");
    }

    #[test]
    fn decodes_the_predefined_entities() {
        assert_eq!(decode_entities("a&amp;b"), "a&b");
        assert_eq!(decode_entities("&lt;&gt;&quot;&apos;"), "<>\"'");
        assert_eq!(decode_entities("&#65;&#x42;"), "AB");
        assert_eq!(decode_entities("plain"), "plain");
        assert_eq!(decode_entities("&unknown;"), "&unknown;");
        assert_eq!(decode_entities("&broken"), "&broken");
        assert_eq!(decode_entities("&#xZZ;"), "&#xZZ;");
    }
}
