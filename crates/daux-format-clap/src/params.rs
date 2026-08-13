//! `clap_param_info` from a DAUx [`ParamInfo`].
//!
//! CLAP parameter values are **plain**, exactly like DAUx's, which makes this the one
//! mapping in the whole adapter that is nearly the identity: `min_value`, `max_value`,
//! `default_value` and every value that crosses an event are real-world units on both
//! sides, and no normalisation happens anywhere. (The VST3 adapter has to normalise
//! everything; CLAP does not, and that difference is the reason `Param` speaks plain.)
//!
//! What does need translating is the flag set. DAUx has one `PER_NOTE` bit; CLAP has six
//! per-scope bits split across automation and modulation, and a host reads them to decide
//! whether it may draw a per-note automation lane at all.
//!
//! `[main-thread]` — building a `ParamInfo` allocates.

use daux_plugin_api::{ParamFlags, ParamInfo};

use crate::abi::{
    CLAP_PARAM_IS_AUTOMATABLE, CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL,
    CLAP_PARAM_IS_AUTOMATABLE_PER_KEY, CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID, CLAP_PARAM_IS_BYPASS,
    CLAP_PARAM_IS_HIDDEN, CLAP_PARAM_IS_MODULATABLE, CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL,
    CLAP_PARAM_IS_MODULATABLE_PER_KEY, CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID,
    CLAP_PARAM_IS_READONLY, CLAP_PARAM_IS_STEPPED, CLAP_PARAM_REQUIRES_PROCESS, ClapParamInfo,
};
use crate::text::write_fixed;

/// `[main-thread]` The CLAP flag set for a DAUx one.
///
/// `PER_NOTE` fans out into all three CLAP per-voice scopes, and only for the directions
/// the parameter actually supports: a parameter that is per-note but not modulatable gets
/// the automation scopes and not the modulation ones, because claiming a scope a plug-in
/// will not honour makes a host draw a lane that does nothing.
///
/// `IS_METER` has no CLAP flag of its own; a meter is expressed as read-only, which is what
/// stops a host from writing to it.
#[must_use]
pub fn param_flags_to_clap(flags: ParamFlags) -> u32 {
    let mut bits = 0u32;
    let automatable = flags.contains(ParamFlags::AUTOMATABLE);
    let modulatable = flags.contains(ParamFlags::MODULATABLE);

    if automatable {
        bits |= CLAP_PARAM_IS_AUTOMATABLE;
    }
    if modulatable {
        bits |= CLAP_PARAM_IS_MODULATABLE;
    }
    if flags.contains(ParamFlags::PER_NOTE) {
        if automatable {
            bits |= CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID
                | CLAP_PARAM_IS_AUTOMATABLE_PER_KEY
                | CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL;
        }
        if modulatable {
            bits |= CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID
                | CLAP_PARAM_IS_MODULATABLE_PER_KEY
                | CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL;
        }
    }
    if flags.contains(ParamFlags::STEPPED) {
        bits |= CLAP_PARAM_IS_STEPPED;
    }
    if flags.contains(ParamFlags::READ_ONLY) || flags.contains(ParamFlags::IS_METER) {
        bits |= CLAP_PARAM_IS_READONLY;
    }
    if flags.contains(ParamFlags::HIDDEN) {
        bits |= CLAP_PARAM_IS_HIDDEN;
    }
    if flags.contains(ParamFlags::BYPASS) {
        bits |= CLAP_PARAM_IS_BYPASS;
    }
    if flags.contains(ParamFlags::REQUIRES_PROCESS) {
        bits |= CLAP_PARAM_REQUIRES_PROCESS;
    }
    bits
}

/// `[main-thread]` Fills a host-owned `clap_param_info` from a DAUx one.
///
/// [`ParamInfo::unit`] has no CLAP counterpart — `clap_param_info` has no unit field — so it
/// reaches the host only through `value_to_text`, which is where `Param::to_text` already
/// appends it. Every other field is copied, and every buffer is fully rewritten so no part
/// of a previous parameter can survive into this one.
pub fn fill_param_info(info: &ParamInfo, out: &mut ClapParamInfo) {
    out.id = info.id.0;
    out.flags = param_flags_to_clap(info.flags);
    out.cookie = core::ptr::null_mut();
    write_fixed(&mut out.name, &info.name);
    write_fixed(&mut out.module, &info.group);
    out.min_value = info.min;
    out.max_value = info.max;
    out.default_value = info.default;
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::c_char;
    use daux_plugin_api::{ParamId, ParamRange};

    fn empty_info() -> ClapParamInfo {
        ClapParamInfo {
            id: u32::MAX,
            flags: u32::MAX,
            cookie: core::ptr::without_provenance_mut(0xdead),
            name: [0x7f as c_char; crate::abi::CLAP_NAME_SIZE],
            module: [0x7f as c_char; crate::abi::CLAP_PATH_SIZE],
            min_value: f64::NAN,
            max_value: f64::NAN,
            default_value: f64::NAN,
        }
    }

    fn read<const N: usize>(buf: &[c_char; N]) -> String {
        let bytes: Vec<u8> = buf
            .iter()
            .take_while(|b| **b != 0)
            .map(|b| *b as u8)
            .collect();
        String::from_utf8(bytes).expect("written text is valid UTF-8")
    }

    #[test]
    fn a_plain_automatable_parameter_gets_exactly_one_flag() {
        assert_eq!(
            param_flags_to_clap(ParamFlags::AUTOMATABLE),
            CLAP_PARAM_IS_AUTOMATABLE
        );
        assert_eq!(param_flags_to_clap(ParamFlags::EMPTY), 0);
    }

    #[test]
    fn per_note_only_claims_the_scopes_the_parameter_supports() {
        let automation_only = param_flags_to_clap(ParamFlags::AUTOMATABLE | ParamFlags::PER_NOTE);
        assert_eq!(
            automation_only,
            CLAP_PARAM_IS_AUTOMATABLE
                | CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID
                | CLAP_PARAM_IS_AUTOMATABLE_PER_KEY
                | CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL
        );
        assert_eq!(
            automation_only & CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID,
            0,
            "a parameter that is not modulatable must not claim a modulation scope"
        );

        let both = param_flags_to_clap(
            ParamFlags::AUTOMATABLE | ParamFlags::MODULATABLE | ParamFlags::PER_NOTE,
        );
        assert_ne!(both & CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID, 0);
        assert_ne!(both & CLAP_PARAM_IS_MODULATABLE_PER_KEY, 0);
        assert_ne!(both & CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL, 0);

        let neither = param_flags_to_clap(ParamFlags::PER_NOTE);
        assert_eq!(
            neither, 0,
            "PER_NOTE on its own says nothing a host can act on"
        );
    }

    #[test]
    fn a_meter_is_expressed_as_read_only() {
        let meter = param_flags_to_clap(ParamFlags::METER_DEFAULT);
        assert_eq!(meter, CLAP_PARAM_IS_READONLY);
        // …and so is anything explicitly marked read-only.
        assert_eq!(
            param_flags_to_clap(ParamFlags::READ_ONLY),
            CLAP_PARAM_IS_READONLY
        );
    }

    #[test]
    fn the_remaining_flags_map_one_for_one() {
        for (daux, clap) in [
            (ParamFlags::STEPPED, CLAP_PARAM_IS_STEPPED),
            (ParamFlags::HIDDEN, CLAP_PARAM_IS_HIDDEN),
            (ParamFlags::BYPASS, CLAP_PARAM_IS_BYPASS),
            (ParamFlags::REQUIRES_PROCESS, CLAP_PARAM_REQUIRES_PROCESS),
            (ParamFlags::MODULATABLE, CLAP_PARAM_IS_MODULATABLE),
        ] {
            assert_eq!(param_flags_to_clap(daux), clap, "{daux:?}");
        }
    }

    #[test]
    fn an_unknown_bit_cannot_leak_into_the_clap_flags() {
        // `from_bits_truncate` drops bits ABI v1 does not define; the mapping must never
        // invent a CLAP flag for one that slipped through another route.
        let flags = ParamFlags::from_bits_truncate(u32::MAX);
        let bits = param_flags_to_clap(flags);
        let known = CLAP_PARAM_IS_STEPPED
            | CLAP_PARAM_IS_HIDDEN
            | CLAP_PARAM_IS_READONLY
            | CLAP_PARAM_IS_BYPASS
            | CLAP_PARAM_IS_AUTOMATABLE
            | CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID
            | CLAP_PARAM_IS_AUTOMATABLE_PER_KEY
            | CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL
            | CLAP_PARAM_IS_MODULATABLE
            | CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID
            | CLAP_PARAM_IS_MODULATABLE_PER_KEY
            | CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL
            | CLAP_PARAM_REQUIRES_PROCESS;
        assert_eq!(bits & !known, 0);
    }

    #[test]
    fn every_field_of_the_host_struct_is_overwritten() {
        let range = ParamRange::Linear {
            min: 20.0,
            max: 20_000.0,
        };
        let info = ParamInfo::new(ParamId(17), "Cutoff", &range, 1_000.0, ParamFlags::DEFAULT)
            .with_group("Filter")
            .with_unit("Hz");
        let mut out = empty_info();
        fill_param_info(&info, &mut out);

        assert_eq!(out.id, 17);
        assert_eq!(read(&out.name), "Cutoff");
        assert_eq!(read(&out.module), "Filter");
        assert_eq!(out.min_value, 20.0);
        assert_eq!(out.max_value, 20_000.0);
        assert_eq!(out.default_value, 1_000.0);
        assert!(out.cookie.is_null(), "a stale cookie would be echoed back");
        assert_ne!(
            out.flags,
            u32::MAX,
            "the flags must be replaced, not merged"
        );
    }

    #[test]
    fn values_stay_plain_and_are_not_normalised() {
        let range = ParamRange::Linear {
            min: -60.0,
            max: 12.0,
        };
        let info = ParamInfo::new(ParamId(1), "Gain", &range, 0.0, ParamFlags::DEFAULT);
        let mut out = empty_info();
        fill_param_info(&info, &mut out);
        assert_eq!(
            (out.min_value, out.max_value, out.default_value),
            (-60.0, 12.0, 0.0),
            "CLAP takes plain values; normalising here would silently rescale every preset"
        );
    }

    #[test]
    fn an_over_long_name_is_truncated_rather_than_overflowing_the_buffer() {
        let long = "n".repeat(crate::abi::CLAP_NAME_SIZE * 2);
        let info = ParamInfo::new(
            ParamId(2),
            &long,
            &ParamRange::Boolean,
            0.0,
            ParamFlags::EMPTY,
        );
        let mut out = empty_info();
        fill_param_info(&info, &mut out);
        assert_eq!(read(&out.name).len(), crate::abi::CLAP_NAME_SIZE - 1);
        assert_eq!(out.name[crate::abi::CLAP_NAME_SIZE - 1], 0);
    }
}
