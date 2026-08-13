//! Upgrading old documents to the current schema.

use crate::doc::StateDoc;
use crate::error::{StateError, StateResult};
use crate::limits::StateLimits;
use crate::version::StateVersion;

/// One migration step: rewrites a document from version `from` to version `from + 1`.
///
/// A plain `fn` pointer rather than a boxed closure, so a chain is `Copy`-cheap to build,
/// `Send + Sync`, and cannot capture per-instance state that would make migration depend
/// on which plug-in instance happened to run it.
pub type MigrationStep = fn(&mut StateDoc) -> StateResult<()>;

/// An ordered chain of `from → from + 1` steps that brings any supported old document up
/// to the current schema version. [main-thread]
///
/// `docs/specifications/abi-v1.md` §12 requires a plug-in to load every schema version it
/// has ever shipped, or fail cleanly with no side effects. Registering one small step per
/// version bump satisfies that: the chain applies them in order, and a document from a
/// version with no registered step is rejected rather than half-loaded.
///
/// Steps that are impossible to satisfy are latched at build time and reported by
/// [`MigrationChain::migrate`], so a mistake in the chain surfaces the first time state is
/// loaded rather than silently doing nothing.
///
/// ```
/// use daux_state::{MigrationChain, StateDoc, StateVersion};
///
/// let chain = MigrationChain::new(StateVersion(3))
///     .step(StateVersion(1), |doc| {
///         // v1 stored the gain in decibels under an old name.
///         doc.rename("gain_db", "gain")
///     })
///     .step(StateVersion(2), |doc| {
///         // v2 gained a bypass switch, defaulted to off.
///         doc.insert("bypass", false)?;
///         Ok(())
///     });
///
/// let mut old = StateDoc::new(StateVersion(1));
/// old.insert("gain_db", -6.0)?;
///
/// let current = chain.migrate(old)?;
/// assert_eq!(current.version(), StateVersion(3));
/// assert_eq!(current.f64("gain")?, -6.0);
/// assert_eq!(current.bool("bypass")?, false);
/// # Ok::<(), daux_state::StateError>(())
/// ```
#[derive(Clone, Debug)]
pub struct MigrationChain {
    current: StateVersion,
    steps: Vec<(u32, MigrationStep)>,
    error: Option<StateError>,
}

impl MigrationChain {
    /// A chain that targets `current`, the version the plug-in writes today.
    /// [main-thread]
    #[must_use]
    pub const fn new(current: StateVersion) -> Self {
        Self {
            current,
            steps: Vec::new(),
            error: None,
        }
    }

    /// Registers the step that turns a `from` document into a `from + 1` document.
    /// [main-thread]
    ///
    /// Consuming so chains read as one expression. A duplicate `from`, or a `from` that is
    /// not below [`MigrationChain::current`], latches an error that
    /// [`MigrationChain::migrate`] returns; the bad step is not registered.
    #[must_use]
    pub fn step(mut self, from: StateVersion, f: MigrationStep) -> Self {
        if from >= self.current {
            self.latch(StateError::migration(format!(
                "step from {from} does not lead anywhere: the current version is {}",
                self.current
            )));
            return self;
        }
        if self.steps.iter().any(|(v, _)| *v == from.get()) {
            self.latch(StateError::migration(format!(
                "two migration steps are registered for {from}"
            )));
            return self;
        }
        self.steps.push((from.get(), f));
        self
    }

    fn latch(&mut self, e: StateError) {
        if self.error.is_none() {
            self.error = Some(e);
        }
    }

    /// The version this chain migrates *to*. [main-thread]
    #[inline]
    #[must_use]
    pub const fn current(&self) -> StateVersion {
        self.current
    }

    /// Number of registered steps. [main-thread]
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// `true` when no step is registered. [main-thread]
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The error latched while the chain was built, if any. [main-thread]
    #[inline]
    #[must_use]
    pub fn error(&self) -> Option<&StateError> {
        self.error.as_ref()
    }

    /// The oldest version this chain can bring forward — the current version when there
    /// are no steps. [main-thread]
    #[must_use]
    pub fn oldest_supported(&self) -> StateVersion {
        let mut oldest = self.current;
        // Walk backwards from `current` while a contiguous step exists.
        while oldest.get() > 0 {
            let previous = StateVersion(oldest.get() - 1);
            if self.steps.iter().any(|(v, _)| *v == previous.get()) {
                oldest = previous;
            } else {
                break;
            }
        }
        oldest
    }

    /// `true` when [`MigrationChain::migrate`] would find a path from `version`.
    /// [main-thread]
    #[must_use]
    pub fn can_migrate_from(&self, version: StateVersion) -> bool {
        version <= self.current && version >= self.oldest_supported()
    }

    /// Applies steps until the document reaches [`MigrationChain::current`].
    /// [main-thread]
    ///
    /// A document already at the current version is returned untouched. A document from a
    /// *newer* version fails with
    /// [`UnsupportedVersion`](crate::StateErrorKind::UnsupportedVersion), as does one whose
    /// version has no registered step. A step that returns an error aborts the whole
    /// migration: the caller gets an `Err` and the partially migrated document is dropped,
    /// which is what makes loading atomic from the host's point of view.
    pub fn migrate(&self, doc: StateDoc) -> StateResult<StateDoc> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let mut doc = doc;
        if doc.version() > self.current {
            return Err(StateError::unsupported_version(
                doc.version().get(),
                self.current.get(),
            ));
        }
        let found = doc.version();
        // Each step strictly increases the version, so this terminates after at most
        // `current - found` iterations.
        while doc.version() < self.current {
            let from = doc.version();
            let Some((_, step)) = self.steps.iter().find(|(v, _)| *v == from.get()) else {
                return Err(
                    StateError::unsupported_version(found.get(), self.current.get())
                        .with_key("<schema>")
                        .at_offset(0),
                );
            };
            step(&mut doc).map_err(|e| {
                StateError::migration(format!("step {from} → {} failed: {e}", from.next()))
            })?;
            doc.set_version(from.next());
        }
        Ok(doc)
    }

    /// Parses `bytes` and migrates the result in one call. [main-thread]
    pub fn migrate_bytes(&self, bytes: &[u8]) -> StateResult<StateDoc> {
        self.migrate_bytes_with_limits(bytes, &StateLimits::DEFAULT)
    }

    /// [`MigrationChain::migrate_bytes`] with explicit limits. [main-thread]
    pub fn migrate_bytes_with_limits(
        &self,
        bytes: &[u8],
        limits: &StateLimits,
    ) -> StateResult<StateDoc> {
        self.migrate(StateDoc::from_bytes_with_limits(bytes, limits)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StateErrorKind, StateVersion, StateWriter};

    /// v1 → v2: the gain moved out of the root and into a group.
    fn v1_to_v2(doc: &mut StateDoc) -> StateResult<()> {
        doc.rename("gain", "output/gain")
    }

    /// v2 → v3: a bypass switch appeared, defaulting to off.
    fn v2_to_v3(doc: &mut StateDoc) -> StateResult<()> {
        doc.insert("bypass", false)?;
        Ok(())
    }

    fn chain() -> MigrationChain {
        MigrationChain::new(StateVersion(3))
            .step(StateVersion(1), v1_to_v2)
            .step(StateVersion(2), v2_to_v3)
    }

    fn v1_doc() -> StateDoc {
        let mut doc = StateDoc::new(StateVersion(1));
        doc.insert("gain", -6.0f64).expect("insert");
        doc
    }

    #[test]
    fn applies_every_step_in_order() {
        let c = chain();
        assert_eq!(c.current(), StateVersion(3));
        assert_eq!(c.len(), 2);
        assert!(!c.is_empty());
        assert!(c.error().is_none());

        let out = c.migrate(v1_doc()).expect("migrates");
        assert_eq!(out.version(), StateVersion(3));
        assert_eq!(out.f64("output/gain"), Ok(-6.0));
        assert_eq!(out.bool("bypass"), Ok(false));
        assert!(!out.contains("gain"));
    }

    #[test]
    fn steps_may_be_registered_out_of_order() {
        let c = MigrationChain::new(StateVersion(3))
            .step(StateVersion(2), v2_to_v3)
            .step(StateVersion(1), v1_to_v2);
        let out = c.migrate(v1_doc()).expect("migrates");
        assert_eq!(out.version(), StateVersion(3));
        assert_eq!(out.f64("output/gain"), Ok(-6.0));
    }

    #[test]
    fn starting_mid_chain_runs_only_the_remaining_steps() {
        let mut doc = StateDoc::new(StateVersion(2));
        doc.insert("output/gain", -3.0f64).expect("insert");
        let out = chain().migrate(doc).expect("migrates");
        assert_eq!(out.version(), StateVersion(3));
        assert_eq!(out.f64("output/gain"), Ok(-3.0));
        assert_eq!(out.bool("bypass"), Ok(false));
    }

    #[test]
    fn a_current_document_is_returned_untouched() {
        let mut doc = StateDoc::new(StateVersion(3));
        doc.insert("bypass", true).expect("insert");
        let out = chain().migrate(doc.clone()).expect("no-op");
        assert_eq!(out, doc);
    }

    #[test]
    fn an_empty_chain_only_accepts_the_current_version() {
        let c = MigrationChain::new(StateVersion(1));
        assert!(c.is_empty());
        assert_eq!(c.oldest_supported(), StateVersion(1));
        assert!(c.migrate(StateDoc::new(StateVersion(1))).is_ok());
        let err = c
            .migrate(StateDoc::new(StateVersion(0)))
            .expect_err("no path");
        assert!(matches!(
            err.kind(),
            StateErrorKind::UnsupportedVersion { .. }
        ));
    }

    #[test]
    fn a_newer_document_is_rejected() {
        let err = chain()
            .migrate(StateDoc::new(StateVersion(4)))
            .expect_err("from the future");
        assert_eq!(
            err.kind(),
            &StateErrorKind::UnsupportedVersion {
                found: 4,
                supported: 3
            }
        );
        assert!(err.to_string().contains('4'));
    }

    #[test]
    fn a_gap_in_the_chain_is_rejected() {
        // No step from v1, so a v1 document cannot reach v3.
        let c = MigrationChain::new(StateVersion(3)).step(StateVersion(2), v2_to_v3);
        assert_eq!(c.oldest_supported(), StateVersion(2));
        assert!(!c.can_migrate_from(StateVersion(1)));
        assert!(c.can_migrate_from(StateVersion(2)));
        assert!(c.can_migrate_from(StateVersion(3)));
        let err = c.migrate(v1_doc()).expect_err("gap");
        assert!(matches!(
            err.kind(),
            StateErrorKind::UnsupportedVersion { .. }
        ));
    }

    #[test]
    fn a_failing_step_aborts_the_whole_migration() {
        fn explodes(_: &mut StateDoc) -> StateResult<()> {
            Err(StateError::missing_field("legacy_gain"))
        }
        let c = MigrationChain::new(StateVersion(3))
            .step(StateVersion(1), explodes)
            .step(StateVersion(2), v2_to_v3);
        let err = c.migrate(v1_doc()).expect_err("step fails");
        assert_eq!(err.kind(), &StateErrorKind::Migration);
        let text = err.to_string();
        assert!(text.contains("v1"), "{text}");
        assert!(text.contains("v2"), "{text}");
        assert!(text.contains("legacy_gain"), "{text}");
    }

    #[test]
    fn a_duplicate_step_is_latched() {
        let c = MigrationChain::new(StateVersion(3))
            .step(StateVersion(1), v1_to_v2)
            .step(StateVersion(1), v1_to_v2);
        assert!(c.error().is_some());
        assert_eq!(c.len(), 1);
        let err = c.migrate(v1_doc()).expect_err("latched");
        assert_eq!(err.kind(), &StateErrorKind::Migration);
        assert!(err.to_string().contains("two migration steps"));
    }

    #[test]
    fn a_step_at_or_above_the_current_version_is_latched() {
        let c = MigrationChain::new(StateVersion(2)).step(StateVersion(2), v2_to_v3);
        assert!(c.error().is_some());
        assert!(c.is_empty());
        assert!(c.migrate(StateDoc::new(StateVersion(2))).is_err());

        let c = MigrationChain::new(StateVersion(2)).step(StateVersion(5), v2_to_v3);
        assert!(c.error().is_some());
    }

    #[test]
    fn oldest_supported_only_counts_a_contiguous_run() {
        let c = MigrationChain::new(StateVersion(5))
            .step(StateVersion(4), v2_to_v3)
            .step(StateVersion(3), v2_to_v3)
            .step(StateVersion(1), v2_to_v3); // v2 missing, so v1 is unreachable
        assert_eq!(c.oldest_supported(), StateVersion(3));
        assert!(!c.can_migrate_from(StateVersion(1)));
        assert!(c.can_migrate_from(StateVersion(3)));
    }

    #[test]
    fn migrates_straight_from_bytes() {
        let mut w = StateWriter::new(StateVersion(1));
        w.put_f64("gain", -12.0);
        let bytes = w.try_finish().expect("valid");

        let out = chain().migrate_bytes(&bytes).expect("migrates");
        assert_eq!(out.version(), StateVersion(3));
        assert_eq!(out.f64("output/gain"), Ok(-12.0));

        // …and refuses hostile bytes before any step runs.
        assert!(chain().migrate_bytes(b"junk").is_err());
        assert!(
            chain()
                .migrate_bytes_with_limits(&bytes, &StateLimits::default().with_max_blob_bytes(4))
                .is_err()
        );
    }

    #[test]
    fn a_long_chain_terminates() {
        fn noop(_: &mut StateDoc) -> StateResult<()> {
            Ok(())
        }
        let mut c = MigrationChain::new(StateVersion(64));
        for v in 1..64u32 {
            c = c.step(StateVersion(v), noop);
        }
        assert!(c.error().is_none());
        let out = c.migrate(StateDoc::new(StateVersion(1))).expect("migrates");
        assert_eq!(out.version(), StateVersion(64));
        assert_eq!(c.oldest_supported(), StateVersion(1));
    }
}
