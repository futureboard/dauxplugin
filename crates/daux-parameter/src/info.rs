//! Host-facing description of one parameter.

use crate::{ParamFlags, ParamId, ParamRange};

/// Everything a host needs to draw, automate and save one parameter.
///
/// This is the Rust mirror of `DauxParamInfoV1` (`abi-v1` §11.2). It owns its strings,
/// so building one allocates: it is produced on the main thread when a host scans or
/// re-scans the parameter list, never inside `process`.
///
/// `min`/`max` are ordered (`min <= max`) even for a deliberately inverted range, and
/// `step_count` counts *intervals*, so a discrete parameter has `step_count + 1`
/// distinct values and `0` means continuous.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ParamInfo {
    /// Permanent id; see [`ParamId`].
    pub id: ParamId,
    /// Display name, e.g. `"Gain"`.
    pub name: String,
    /// Group path, `""` for top level and `/`-separated otherwise, e.g. `"Filter/Env"`.
    pub group: String,
    /// Unit suffix, e.g. `"dB"`, `"Hz"`, `"%"`, or `""`.
    pub unit: String,
    /// What the host may do with the parameter.
    pub flags: ParamFlags,
    /// Number of intervals between discrete values; `0` for continuous parameters.
    pub step_count: u32,
    /// Lowest plain value.
    pub min: f64,
    /// Highest plain value.
    pub max: f64,
    /// Plain value restored by [`Param::reset`](crate::Param::reset).
    pub default: f64,
}

impl ParamInfo {
    /// `[main-thread]` Builds the info for a parameter with the given range.
    ///
    /// `min`, `max`, `step_count` and the `STEPPED` flag are all derived from `range`,
    /// which is the only way to keep them consistent with the curve the parameter
    /// actually uses.
    #[must_use]
    pub fn new(
        id: ParamId,
        name: impl Into<String>,
        range: &ParamRange,
        default: f64,
        flags: ParamFlags,
    ) -> Self {
        let (min, max) = range.bounds();
        let mut flags = flags;
        if range.is_stepped() {
            flags.insert(ParamFlags::STEPPED);
        }
        Self {
            id,
            name: name.into(),
            group: String::new(),
            unit: String::new(),
            flags,
            step_count: range.step_count(),
            min,
            max,
            default,
        }
    }

    /// `[main-thread]` Sets the group path.
    #[must_use]
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// `[main-thread]` Sets the unit suffix.
    #[must_use]
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = unit.into();
        self
    }

    /// `[any-thread]` True when the parameter has discrete values.
    #[must_use]
    pub fn is_stepped(&self) -> bool {
        self.step_count > 0 || self.flags.is_stepped()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_bounds_and_steps_from_the_range() {
        let info = ParamInfo::new(
            ParamId(3),
            "Mode",
            &ParamRange::stepped(0, 4),
            2.0,
            ParamFlags::AUTOMATABLE,
        )
        .with_group("Filter")
        .with_unit("");

        assert_eq!(info.id, ParamId(3));
        assert_eq!(info.name, "Mode");
        assert_eq!(info.group, "Filter");
        assert_eq!(info.step_count, 4);
        assert_eq!(info.min, 0.0);
        assert_eq!(info.max, 4.0);
        assert_eq!(info.default, 2.0);
        assert!(
            info.flags
                .contains(ParamFlags::STEPPED | ParamFlags::AUTOMATABLE)
        );
        assert!(info.is_stepped());
    }

    #[test]
    fn continuous_parameters_report_no_steps() {
        let info = ParamInfo::new(
            ParamId(1),
            "Gain",
            &ParamRange::linear(-60.0, 12.0),
            0.0,
            ParamFlags::DEFAULT,
        )
        .with_unit("dB");
        assert_eq!(info.step_count, 0);
        assert!(!info.flags.is_stepped());
        assert!(!info.is_stepped());
        assert_eq!(info.unit, "dB");
        assert_eq!(info.group, "");
    }

    #[test]
    fn inverted_ranges_are_reported_in_order() {
        let info = ParamInfo::new(
            ParamId(1),
            "Reverse",
            &ParamRange::linear(1.0, -1.0),
            0.0,
            ParamFlags::EMPTY,
        );
        assert_eq!(info.min, -1.0);
        assert_eq!(info.max, 1.0);
    }
}
