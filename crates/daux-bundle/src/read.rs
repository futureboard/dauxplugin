//! Turning untrusted bytes into text, per `manifest-v1` §10.2.

use crate::{BundleError, BundleErrorKind, BundleResult};

/// The UTF-8 byte-order mark, which may be skipped.
const BOM_UTF8: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Byte-order marks that identify an encoding this format does not accept, with the name to
/// report. UTF-32 must be tested before UTF-16LE, whose mark is a prefix of it.
const REJECTED_BOMS: [(&[u8], &str); 4] = [
    (&[0x00, 0x00, 0xFE, 0xFF], "UTF-32BE"),
    (&[0xFF, 0xFE, 0x00, 0x00], "UTF-32LE"),
    (&[0xFE, 0xFF], "UTF-16BE"),
    (&[0xFF, 0xFE], "UTF-16LE"),
];

/// Decodes metadata bytes as UTF-8, borrowing rather than copying.
///
/// A leading UTF-8 BOM is skipped: editors on Windows add one and refusing the file over it
/// would be pedantry. A UTF-16 or UTF-32 BOM is rejected rather than transcoded — accepting
/// one would mean every downstream length limit in [`limits`](crate::limits) counts a
/// different unit than the one the specification fixes.
///
/// # Errors
///
/// [`BundleErrorKind::Encoding`] for an unsupported encoding or for bytes that are not valid
/// UTF-8. Never panics.
pub(crate) fn decode_utf8(bytes: &[u8]) -> BundleResult<&str> {
    for (bom, name) in REJECTED_BOMS {
        if bytes.starts_with(bom) {
            return Err(BundleError::new(
                BundleErrorKind::Encoding,
                format!("metadata is {name}; only UTF-8 is supported"),
            ));
        }
    }

    let body = bytes.strip_prefix(BOM_UTF8).unwrap_or(bytes);
    core::str::from_utf8(body).map_err(|e| {
        BundleError::new(
            BundleErrorKind::Encoding,
            format!("metadata is not valid UTF-8: {e}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_utf8_is_returned_as_is() {
        assert_eq!(decode_utf8(b"{}").unwrap(), "{}");
        assert_eq!(decode_utf8("héllo".as_bytes()).unwrap(), "héllo");
        assert_eq!(decode_utf8(b"").unwrap(), "");
    }

    #[test]
    fn a_utf8_bom_is_skipped() {
        let mut bytes = BOM_UTF8.to_vec();
        bytes.extend_from_slice(b"{\"a\":1}");
        assert_eq!(decode_utf8(&bytes).unwrap(), "{\"a\":1}");
    }

    #[test]
    fn a_bom_alone_decodes_to_nothing() {
        assert_eq!(decode_utf8(BOM_UTF8).unwrap(), "");
    }

    #[test]
    fn wide_encodings_are_rejected_by_name() {
        for (bom, name) in REJECTED_BOMS {
            let mut bytes = bom.to_vec();
            bytes.extend_from_slice(&[0x7B, 0x00, 0x7D, 0x00]);
            let err = decode_utf8(&bytes).unwrap_err();
            assert_eq!(*err.kind(), BundleErrorKind::Encoding);
            assert!(
                err.detail().is_some_and(|d| d.contains(name)),
                "{name} must be named in the diagnostic"
            );
        }
    }

    #[test]
    fn utf32le_is_not_mistaken_for_utf16le() {
        // The UTF-16LE mark is a prefix of the UTF-32LE one; order matters.
        let err = decode_utf8(&[0xFF, 0xFE, 0x00, 0x00, 0x7B]).unwrap_err();
        assert!(err.detail().is_some_and(|d| d.contains("UTF-32LE")));
    }

    #[test]
    fn invalid_utf8_is_reported_rather_than_replaced() {
        let err = decode_utf8(&[0x7B, 0xC3, 0x28, 0x7D]).unwrap_err();
        assert_eq!(*err.kind(), BundleErrorKind::Encoding);
        // A lone continuation byte, not a BOM: still an encoding error, not a panic.
        assert!(decode_utf8(&[0x80]).is_err());
    }
}
