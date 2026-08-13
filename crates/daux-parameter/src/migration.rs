//! Parameter renames and removals across plug-in versions.

use crate::ParamId;

/// One step in the story of how a plug-in's parameter list changed.
///
/// Parameter ids are permanent (`abi-v1` §14): a saved project stores ids, so
/// renumbering one silently loads the wrong value into the wrong control. When a
/// parameter genuinely has to move or disappear, the plug-in records it here and the
/// state layer replays the chain while loading older state.
///
/// ```
/// use daux_parameter::{ParamId, ParamMigration, migrate_param_id};
///
/// // v2 split "tone" into "tilt", and dropped the old "legacy mode" switch.
/// let chain = [
///     ParamMigration::rename(ParamId(3), ParamId(7)),
///     ParamMigration::removed(ParamId(4)),
/// ];
///
/// assert_eq!(migrate_param_id(&chain, ParamId(3)), Some(ParamId(7)));
/// assert_eq!(migrate_param_id(&chain, ParamId(4)), None);
/// assert_eq!(migrate_param_id(&chain, ParamId(9)), Some(ParamId(9)));
/// ```
///
/// `[any-thread]` — plain `Copy` data, built at compile time or during load, never in
/// `process`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ParamMigration {
    old: ParamId,
    new: Option<ParamId>,
}

impl ParamMigration {
    /// `[any-thread]` The parameter `old` is now known as `new`.
    ///
    /// The value carries over unchanged, so this is only for identity, not for units:
    /// a parameter that changed meaning needs a new id and a state-level conversion.
    #[must_use]
    pub const fn rename(old: ParamId, new: ParamId) -> Self {
        Self {
            old,
            new: Some(new),
        }
    }

    /// `[any-thread]` The parameter `id` no longer exists; saved values are dropped.
    #[must_use]
    pub const fn removed(id: ParamId) -> Self {
        Self { old: id, new: None }
    }

    /// `[any-thread]` The id as it appears in old saved state.
    #[inline]
    #[must_use]
    pub const fn old_id(self) -> ParamId {
        self.old
    }

    /// `[any-thread]` The id it maps to now, or `None` for a removal.
    #[inline]
    #[must_use]
    pub const fn new_id(self) -> Option<ParamId> {
        self.new
    }

    /// `[any-thread]` True when this step drops the parameter.
    #[inline]
    #[must_use]
    pub const fn is_removal(self) -> bool {
        self.new.is_none()
    }

    /// `[any-thread]` True when this step renames the parameter.
    #[inline]
    #[must_use]
    pub const fn is_rename(self) -> bool {
        self.new.is_some()
    }

    /// `[any-thread]` Applies this step to one saved id.
    ///
    /// Returns the id to use now, or `None` when the parameter was removed. Ids this
    /// step says nothing about come back unchanged, which is what lets a whole chain be
    /// folded with [`Option::and_then`].
    #[inline]
    #[must_use]
    pub fn apply(self, saved: ParamId) -> Option<ParamId> {
        if saved == self.old {
            self.new
        } else {
            Some(saved)
        }
    }
}

/// `[any-thread]` Folds a whole migration chain over one saved id.
///
/// Steps are applied in order, so `rename(1 → 2)` followed by `rename(2 → 3)` maps a
/// saved `1` to `3`. Returns `None` as soon as a step removes the parameter, which
/// tells the state layer to drop the stored value instead of writing it somewhere
/// wrong.
#[must_use]
pub fn migrate_param_id(chain: &[ParamMigration], saved: ParamId) -> Option<ParamId> {
    chain.iter().try_fold(saved, |id, step| step.apply(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_maps_the_old_id_and_leaves_others_alone() {
        let m = ParamMigration::rename(ParamId(3), ParamId(7));
        assert_eq!(m.old_id(), ParamId(3));
        assert_eq!(m.new_id(), Some(ParamId(7)));
        assert!(m.is_rename());
        assert!(!m.is_removal());
        assert_eq!(m.apply(ParamId(3)), Some(ParamId(7)));
        assert_eq!(m.apply(ParamId(4)), Some(ParamId(4)));
    }

    #[test]
    fn a_removal_drops_only_its_own_id() {
        let m = ParamMigration::removed(ParamId(4));
        assert_eq!(m.old_id(), ParamId(4));
        assert_eq!(m.new_id(), None);
        assert!(m.is_removal());
        assert!(!m.is_rename());
        assert_eq!(m.apply(ParamId(4)), None);
        assert_eq!(m.apply(ParamId(5)), Some(ParamId(5)));
    }

    #[test]
    fn chains_apply_in_order() {
        let chain = [
            ParamMigration::rename(ParamId(1), ParamId(2)),
            ParamMigration::rename(ParamId(2), ParamId(3)),
        ];
        assert_eq!(migrate_param_id(&chain, ParamId(1)), Some(ParamId(3)));
        assert_eq!(migrate_param_id(&chain, ParamId(2)), Some(ParamId(3)));
        assert_eq!(migrate_param_id(&chain, ParamId(3)), Some(ParamId(3)));
    }

    #[test]
    fn a_removal_later_in_the_chain_wins() {
        let chain = [
            ParamMigration::rename(ParamId(1), ParamId(2)),
            ParamMigration::removed(ParamId(2)),
        ];
        assert_eq!(migrate_param_id(&chain, ParamId(1)), None);
        assert_eq!(migrate_param_id(&chain, ParamId(5)), Some(ParamId(5)));
    }

    #[test]
    fn a_rename_after_a_removal_cannot_resurrect_it() {
        let chain = [
            ParamMigration::removed(ParamId(1)),
            ParamMigration::rename(ParamId(1), ParamId(2)),
        ];
        assert_eq!(migrate_param_id(&chain, ParamId(1)), None);
    }

    #[test]
    fn an_empty_chain_changes_nothing() {
        assert_eq!(migrate_param_id(&[], ParamId(42)), Some(ParamId(42)));
    }

    #[test]
    fn a_self_rename_is_harmless() {
        let chain = [ParamMigration::rename(ParamId(1), ParamId(1))];
        assert_eq!(migrate_param_id(&chain, ParamId(1)), Some(ParamId(1)));
    }

    #[test]
    fn is_usable_in_const_context() {
        const CHAIN: [ParamMigration; 2] = [
            ParamMigration::rename(ParamId(3), ParamId(7)),
            ParamMigration::removed(ParamId(4)),
        ];
        assert_eq!(migrate_param_id(&CHAIN, ParamId(3)), Some(ParamId(7)));
        assert_eq!(migrate_param_id(&CHAIN, ParamId(4)), None);
    }
}
