//! The object-safe [`Param`] trait and the [`Params`] collection trait.

use crate::{ParamFlags, ParamId, ParamInfo};

/// `[main-thread]` Custom value formatter installed with `with_formatter`.
///
/// A plain function pointer rather than a boxed closure: it keeps every parameter type
/// `Send + Sync + Sized` without a heap allocation, and a non-capturing closure coerces
/// to it automatically. The implementation must replace the contents of `out` and must
/// not allocate anything other than that buffer's growth.
pub type FormatFn = fn(plain: f64, out: &mut String);

/// `[main-thread]` Custom text parser installed with `with_parser`.
///
/// Returns the plain value the text denotes, or `None` to reject the entry and leave
/// the parameter untouched. The value is clamped to the parameter's range afterwards,
/// so a parser never has to range-check.
pub type ParseFn = fn(text: &str) -> Option<f64>;

/// One automatable value, erased to a trait object.
///
/// The trait is deliberately object-safe: hosts, editors and the state layer all work
/// with `&dyn Param`, and a plug-in's parameter list is a heterogeneous mix of
/// [`FloatParam`](crate::FloatParam), [`IntParam`](crate::IntParam),
/// [`BoolParam`](crate::BoolParam), [`EnumParam`](crate::EnumParam) and
/// [`MeterParam`](crate::MeterParam).
///
/// # Values are plain
///
/// `plain`/`set_plain` speak in real-world units — dB, Hz, semitones, an enum index.
/// Those are the values that cross the ABI and land in a saved project.
/// `normalized`/`set_normalized` exist for knobs and for host automation lanes, and go
/// through the parameter's [`ParamRange`](crate::ParamRange). Because normalisation
/// never leaves the plug-in, changing a curve in version 2 of a plug-in cannot corrupt
/// automation written by version 1.
///
/// # Threads
///
/// Value access is `[any-thread]`: it is a single relaxed atomic load or store, so the
/// audio thread, the UI thread and a host automation thread may all touch the same
/// parameter concurrently without locking. Everything that builds a `String` is
/// `[main-thread]`.
pub trait Param: Send + Sync {
    /// `[main-thread]` Full description for the host. Allocates; call it while
    /// scanning or when the host asks, never inside `process`.
    fn info(&self) -> ParamInfo;

    /// `[any-thread]` Current value in real-world units.
    fn plain(&self) -> f64;

    /// `[any-thread]` Stores a real-world value, clamped and quantised to the range.
    ///
    /// Atomic and wait-free: a store from the UI thread is visible to the next audio
    /// block without a lock.
    fn set_plain(&self, v: f64);

    /// `[any-thread]` Current value mapped to `0..=1` through the range's curve.
    fn normalized(&self) -> f64;

    /// `[any-thread]` Stores a `0..=1` position, mapping it back through the curve.
    fn set_normalized(&self, v: f64);

    /// `[main-thread]` Formats `plain` for display, replacing the contents of `out`.
    ///
    /// Allocates nothing beyond growing `out`, so a caller that reuses one
    /// `String::with_capacity(32)` across a whole parameter list allocates once.
    fn to_text(&self, plain: f64, out: &mut String);

    /// `[main-thread]` Parses user input into a plain value, or `None` to reject it.
    ///
    /// Leading and trailing whitespace and a unit suffix are tolerated.
    // `from_*` conventionally takes no `self`, but this is the inverse of `to_text` and
    // genuinely needs the parameter's range, unit and labels. The name is fixed by
    // `docs/architecture/crate-contracts.md`.
    #[allow(clippy::wrong_self_convention)]
    fn from_text(&self, text: &str) -> Option<f64>;

    /// `[any-thread]` Restores the parameter's default value.
    fn reset(&self);

    /// `[any-thread]` This parameter's permanent id.
    ///
    /// The default implementation goes through [`info`](Param::info) and therefore
    /// allocates, which makes it `[main-thread]`; every concrete type in this crate
    /// overrides it with a field read and is genuinely `[any-thread]`.
    fn id(&self) -> ParamId {
        self.info().id
    }

    /// `[any-thread]` This parameter's flags.
    ///
    /// Same caveat as [`id`](Param::id): the default implementation allocates, the
    /// concrete types do not.
    fn flags(&self) -> ParamFlags {
        self.info().flags
    }

    /// `[main-thread]` Convenience wrapper around [`to_text`](Param::to_text) that
    /// allocates a fresh `String`. Editors that render every frame should call
    /// `to_text` with a reused buffer instead.
    fn text(&self, plain: f64) -> String {
        let mut out = String::new();
        self.to_text(plain, &mut out);
        out
    }

    /// `[main-thread]` Parses `text` and stores it, reporting whether it was accepted.
    fn set_from_text(&self, text: &str) -> bool {
        match self.from_text(text) {
            Some(v) => {
                self.set_plain(v);
                true
            }
            None => false,
        }
    }
}

/// A plug-in's parameter list.
///
/// Implemented by hand or generated by `#[derive(DauxParams)]`. The order of
/// [`param_refs`](Params::param_refs) is the order the host shows and, more
/// importantly, the order indices are assigned in — it must be stable for the lifetime
/// of the plug-in id, because hosts remember parameter *indices* even though DAUx
/// itself only ever addresses parameters by [`ParamId`].
pub trait Params: Send + Sync {
    /// `[main-thread]` Every parameter in stable order. Allocates.
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)>;

    /// `[any-thread]` Looks one parameter up by id.
    ///
    /// The default implementation walks [`param_refs`](Params::param_refs) and
    /// therefore allocates, which downgrades it to `[main-thread]`. Any implementation
    /// that is reachable from the audio thread — as the derive macro's is — must
    /// override this with a match or a lookup table.
    fn param(&self, id: ParamId) -> Option<&dyn Param> {
        self.param_refs()
            .into_iter()
            .find_map(|(pid, param)| (pid == id).then_some(param))
    }

    /// `[any-thread]` Schema version of the parameter set, written into saved state so
    /// that a future version knows which [`ParamMigration`](crate::ParamMigration)s to
    /// apply.
    fn state_schema_version(&self) -> u32 {
        1
    }

    /// `[main-thread]` Renames and removals to apply when loading state saved by an
    /// older version. Empty by default.
    fn migrations(&self) -> &[crate::ParamMigration] {
        &[]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BoolParam, FloatParam, ParamRange};

    struct Bank {
        gain: FloatParam,
        invert: BoolParam,
    }

    impl Params for Bank {
        fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
            vec![
                (self.gain.id(), &self.gain as &dyn Param),
                (self.invert.id(), &self.invert as &dyn Param),
            ]
        }
    }

    fn bank() -> Bank {
        Bank {
            gain: FloatParam::new(
                ParamId(1),
                "Gain",
                0.0,
                ParamRange::Linear {
                    min: -60.0,
                    max: 12.0,
                },
            )
            .with_unit("dB"),
            invert: BoolParam::new(ParamId(2), "Invert", false),
        }
    }

    #[test]
    fn default_lookup_finds_parameters_by_id() {
        let bank = bank();
        assert_eq!(bank.param(ParamId(1)).map(Param::id), Some(ParamId(1)));
        assert_eq!(bank.param(ParamId(2)).map(Param::id), Some(ParamId(2)));
        assert!(bank.param(ParamId(3)).is_none());
    }

    #[test]
    fn order_is_stable() {
        let bank = bank();
        let first: Vec<ParamId> = bank.param_refs().iter().map(|(id, _)| *id).collect();
        let second: Vec<ParamId> = bank.param_refs().iter().map(|(id, _)| *id).collect();
        assert_eq!(first, vec![ParamId(1), ParamId(2)]);
        assert_eq!(first, second);
    }

    #[test]
    fn defaults_are_sane() {
        let bank = bank();
        assert_eq!(bank.state_schema_version(), 1);
        assert!(bank.migrations().is_empty());
    }

    #[test]
    fn provided_methods_work_through_the_trait_object() {
        let bank = bank();
        let gain: &dyn Param = bank.param(ParamId(1)).expect("gain exists");
        assert!(gain.flags().is_automatable());
        assert!(gain.set_from_text("-6 dB"));
        assert_eq!(gain.plain(), -6.0);
        assert_eq!(gain.text(gain.plain()), "-6.00 dB");
        assert!(!gain.set_from_text("nonsense"));
        assert_eq!(
            gain.plain(),
            -6.0,
            "a rejected entry must not change the value"
        );
    }

    #[test]
    fn params_is_object_safe_and_shareable() {
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn Param>();
        assert_send_sync::<dyn Params>();

        let bank: std::sync::Arc<dyn Params> = std::sync::Arc::new(bank());
        let clone = std::sync::Arc::clone(&bank);
        let handle = std::thread::spawn(move || {
            clone.param(ParamId(1)).expect("gain exists").set_plain(3.0);
        });
        handle.join().expect("worker thread");
        assert_eq!(bank.param(ParamId(1)).expect("gain exists").plain(), 3.0);
    }
}
