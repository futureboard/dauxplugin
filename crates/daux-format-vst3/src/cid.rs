//! Deriving a VST3 class id from a permanent DAUx plug-in id.
//!
//! VST3 identifies a plug-in by sixteen opaque bytes; DAUx identifies it by a reverse-DNS
//! string. A host stores the sixteen bytes in the user's project, so the mapping between the
//! two is **permanent**: change it and every saved session stops finding the plug-in it was
//! made with. It is therefore a pure function of the id string, defined here once, and
//! covered by a test with a literal expected value so that an "improvement" to the hash
//! cannot land quietly.
//!
//! # Why not the SDK's byte order
//!
//! [`crate::com::uid`] reproduces Steinberg's platform-dependent `INLINE_UID` encoding,
//! because interface ids have to match the host's byte-for-byte. Class ids do **not** go
//! through it: the bytes computed here are used verbatim on every platform, so a project
//! saved on Windows opens on macOS. That is the same choice every cross-platform VST3 plug-in
//! makes, and it is why `daux` never prints a class id in `INLINE_UID` form.

use crate::com::TUid;

/// FNV-1a's 128-bit offset basis.
const FNV_OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
/// FNV-1a's 128-bit prime.
const FNV_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013B;

/// Namespace prefix mixed into every hash.
///
/// Without it, a DAUx plug-in and some other project that also hashes reverse-DNS ids with
/// FNV-1a would produce the same class id for the same string. Changing this constant
/// renumbers every plug-in in existence, so it never changes.
const NAMESPACE: &[u8] = b"daux.vst3.class/1\0";

/// `[main-thread]` The permanent VST3 class id of a DAUx plug-in id.
///
/// A 128-bit FNV-1a hash of `NAMESPACE || id`, stamped with the RFC 4122 version-5 and
/// variant bits so that hosts and log files display it as a well-formed UUID. Those six bits
/// cost nothing: 122 bits of hash still make an accidental collision between two plug-ins
/// impossible in practice.
#[must_use]
pub fn class_id(id: &str) -> TUid {
    let mut hash = FNV_OFFSET;
    for &byte in NAMESPACE.iter().chain(id.as_bytes()) {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let mut bytes = hash.to_be_bytes();
    // RFC 4122 §4.3: version 5 in the high nibble of byte 6, variant `10` in byte 8.
    bytes[6] = (bytes[6] & 0x0F) | 0x50;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    bytes
}

/// `[main-thread]` The class id of the edit controller that belongs to `id`.
///
/// This adapter keeps the component and the controller in one object, so the value is only
/// ever reported through `IComponent::getControllerClassId` for hosts that insist on asking.
/// It is derived from a different namespace suffix so it can never collide with a component
/// id.
#[must_use]
pub fn controller_class_id(id: &str) -> TUid {
    let mut hash = FNV_OFFSET;
    for &byte in NAMESPACE
        .iter()
        .chain(id.as_bytes())
        .chain(b"/controller".iter())
    {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let mut bytes = hash.to_be_bytes();
    bytes[6] = (bytes[6] & 0x0F) | 0x50;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    bytes
}

/// `[main-thread]` Formats a class id the way hosts and log files show it.
#[must_use]
pub fn format_class_id(cid: &TUid) -> String {
    let mut out = String::with_capacity(36);
    for (i, byte) in cid.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{byte:02X}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one test in this crate that must never be "fixed" by updating the expected value.
    /// If the hash changes, every project made with a DAUx VST3 build stops loading.
    #[test]
    fn class_ids_are_frozen() {
        assert_eq!(
            format_class_id(&class_id("com.example.gain")),
            "8AE3F210-BE35-526D-9994-D27F3EC70B43"
        );
        assert_eq!(
            format_class_id(&class_id("studio.futureboard.reverb")),
            "F2469B53-503C-5A21-9786-A3F135503BFE"
        );
        assert_eq!(
            format_class_id(&controller_class_id("com.example.gain")),
            "6DE641B9-9F35-5F88-90F5-6EA8947E06D4"
        );
    }

    #[test]
    fn a_class_id_is_a_well_formed_version_5_uuid() {
        for id in ["com.example.gain", "studio.futureboard.reverb", ""] {
            let cid = class_id(id);
            assert_eq!(cid[6] & 0xF0, 0x50, "version nibble for `{id}`");
            assert_eq!(cid[8] & 0xC0, 0x80, "variant bits for `{id}`");
            assert_eq!(format_class_id(&cid).len(), 36);
        }
    }

    #[test]
    fn different_ids_produce_different_class_ids() {
        let a = class_id("com.example.gain");
        let b = class_id("com.example.gaim");
        let c = class_id("com.example.gain2");
        assert_ne!(a, b, "a one-character difference must change the id");
        assert_ne!(a, c);
        assert_ne!(b, c);
        // …and the controller id never collides with any component id.
        assert_ne!(controller_class_id("com.example.gain"), a);
        assert_ne!(controller_class_id("com.example.gain"), b);
    }

    #[test]
    fn the_derivation_is_a_pure_function() {
        let once = class_id("studio.futureboard.analyzer");
        let twice = class_id("studio.futureboard.analyzer");
        assert_eq!(once, twice);
    }

    #[test]
    fn a_thousand_realistic_ids_do_not_collide() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..1000 {
            let id = format!("com.example.plugin{i}");
            assert!(seen.insert(class_id(&id)), "collision at `{id}`");
        }
        assert_eq!(seen.len(), 1000);
    }
}
