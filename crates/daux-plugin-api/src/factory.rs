//! Turning plug-in *types* into a [`DauxFactory`] a format adapter can enumerate.
//!
//! A factory is what a scanner touches first, so both implementations here are cheap to
//! construct and load nothing: [`SingleFactory`] holds no state at all, and
//! [`PluginRegistry`] holds one descriptor and one function pointer per plug-in.

use core::fmt;
use core::marker::PhantomData;

use daux_core::{DauxFactory, DauxPlugin, DauxResult, ErrorKind, PluginDescriptor};

/// The overwhelmingly common case: a module that exports exactly one plug-in. See the
/// [crate documentation](crate#writing-a-plug-in) for a worked example.
///
/// `P` must be [`Default`] because the ABI's `create` takes no arguments: everything a
/// plug-in needs it must be able to build for itself (abi-v1 §5).
pub struct SingleFactory<P: DauxPlugin + Default> {
    // `fn() -> P` rather than `P`, so the factory is `Send + Sync` regardless of what `P`
    // contains — it never holds one.
    _plugin: PhantomData<fn() -> P>,
}

impl<P: DauxPlugin + Default> SingleFactory<P> {
    /// [main-thread] Builds the factory. Allocation-free and does no work.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            _plugin: PhantomData,
        }
    }
}

impl<P: DauxPlugin + Default> Default for SingleFactory<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: DauxPlugin + Default> Clone for SingleFactory<P> {
    // Hand-written rather than derived: `#[derive(Clone)]` would demand `P: Clone`, and a
    // factory never holds a `P` to clone.
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: DauxPlugin + Default> Copy for SingleFactory<P> {}

impl<P: DauxPlugin + Default> fmt::Debug for SingleFactory<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SingleFactory")
            .field("id", &P::descriptor().id.as_str())
            .finish()
    }
}

impl<P: DauxPlugin + Default> DauxFactory for SingleFactory<P> {
    fn plugin_count(&self) -> usize {
        1
    }

    fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
        (index == 0).then(P::descriptor)
    }

    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
        let descriptor = P::descriptor();
        if descriptor.id == *id {
            Ok(Box::new(P::default()))
        } else {
            Err(ErrorKind::NotFound.error(format!(
                "no plug-in `{id}` in this module; it exports `{}`",
                descriptor.id.as_str()
            )))
        }
    }
}

/// One registered plug-in: its descriptor and the constructor that produces it.
struct Entry {
    descriptor: PluginDescriptor,
    create: fn() -> Box<dyn DauxPlugin>,
}

/// A module that exports several plug-ins. See the
/// [crate documentation](crate#writing-a-plug-in) for a worked example.
///
/// # Duplicate ids
///
/// A plug-in id is permanent and is what a host stores in a project file (abi-v1 §14), so two
/// plug-ins sharing one is not a preference that can be resolved by "last wins" — it means
/// one of them can never be loaded again, and a saved session silently opens the wrong
/// thing. [`try_register`](Self::try_register) rejects it, and
/// [`register`](Self::register) panics on it, at factory-construction time, before any host
/// has seen either descriptor.
#[derive(Default)]
pub struct PluginRegistry {
    entries: Vec<Entry>,
}

impl fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginRegistry")
            .field(
                "ids",
                &self
                    .entries
                    .iter()
                    .map(|e| e.descriptor.id.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl PluginRegistry {
    /// [main-thread] An empty registry.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// [main-thread] Adds `P`, chaining.
    ///
    /// # Panics
    ///
    /// If `P`'s descriptor is invalid, or if its id is already registered. Both are bugs in
    /// the plug-in module rather than run-time conditions, and both are caught while the
    /// factory is being built — inside the adapter's `catch_unwind`, long before any audio
    /// runs. Use [`try_register`](Self::try_register) where a failure must be reported
    /// instead.
    pub fn register<P: DauxPlugin + Default>(&mut self) -> &mut Self {
        match self.try_register::<P>() {
            Ok(this) => this,
            Err(err) => panic!("PluginRegistry::register: {err}"),
        }
    }

    /// [main-thread] Adds `P`, reporting a clash rather than panicking.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidArgument`] when a plug-in with the same permanent id is already
    /// registered, or when `P`'s descriptor fails [`PluginDescriptor::validate`].
    pub fn try_register<P: DauxPlugin + Default>(&mut self) -> DauxResult<&mut Self> {
        let descriptor = P::descriptor();
        descriptor.validate()?;
        if self.contains(descriptor.id.as_str()) {
            return Err(ErrorKind::InvalidArgument.error(format!(
                "plug-in id `{}` is already registered; ids are permanent and must be unique \
                 within a module (abi-v1 §14)",
                descriptor.id.as_str()
            )));
        }
        self.entries.push(Entry {
            descriptor,
            create: || Box::new(P::default()),
        });
        Ok(self)
    }

    /// [main-thread] Adds `P` and returns the registry by value, for building one in an
    /// expression: `PluginRegistry::new().with::<A>().with::<B>()`.
    ///
    /// # Panics
    ///
    /// As [`register`](Self::register).
    #[must_use]
    pub fn with<P: DauxPlugin + Default>(mut self) -> Self {
        self.register::<P>();
        self
    }

    /// [main-thread] How many plug-ins are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// [main-thread] `true` when nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// [main-thread] The registered ids, in registration order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.descriptor.id.as_str())
    }
}

impl DauxFactory for PluginRegistry {
    fn plugin_count(&self) -> usize {
        self.entries.len()
    }

    fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
        self.entries.get(index).map(|e| e.descriptor.clone())
    }

    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
        self.entries
            .iter()
            .find(|e| e.descriptor.id == *id)
            .map(|e| (e.create)())
            .ok_or_else(|| ErrorKind::NotFound.error(format!("no plug-in `{id}` in this module")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Counts, Spy};
    use daux_audio::BusLayout;
    use daux_core::{DauxController, DauxProcessor, ProcessConfig};
    use std::sync::Arc;

    /// A `Default` plug-in wrapping the spy, since `DauxFactory` needs `Default`.
    struct Counted(Spy);

    impl Default for Counted {
        fn default() -> Self {
            Self(Spy::new(Arc::new(Counts::default())))
        }
    }

    impl DauxPlugin for Counted {
        fn descriptor() -> PluginDescriptor {
            Spy::descriptor()
        }
        fn bus_layout(&self) -> BusLayout {
            self.0.bus_layout()
        }
        fn processor(&mut self) -> &mut dyn DauxProcessor {
            self.0.processor()
        }
        fn controller(&mut self) -> &mut dyn DauxController {
            self.0.controller()
        }
    }

    /// A second, distinct plug-in.
    #[derive(Default)]
    struct Other(Option<Spy>);

    impl Other {
        const ID: &'static str = "com.example.other";

        fn spy(&mut self) -> &mut Spy {
            self.0
                .get_or_insert_with(|| Spy::new(Arc::new(Counts::default())))
        }
    }

    impl DauxPlugin for Other {
        fn descriptor() -> PluginDescriptor {
            PluginDescriptor::builder(Self::ID, "Other")
                .build()
                .expect("valid")
        }
        fn bus_layout(&self) -> BusLayout {
            BusLayout::mono_effect()
        }
        fn processor(&mut self) -> &mut dyn DauxProcessor {
            self.spy().processor()
        }
        fn controller(&mut self) -> &mut dyn DauxController {
            self.spy().controller()
        }
    }

    /// A plug-in whose descriptor is corrupted after the builder validated it. The fields of
    /// `PluginDescriptor` are public, so this is reachable by accident — a plug-in that
    /// clears its own name, say — and a factory must not publish it.
    #[derive(Default)]
    struct Nameless(Other);

    impl DauxPlugin for Nameless {
        fn descriptor() -> PluginDescriptor {
            let mut d = PluginDescriptor::builder("com.example.nameless", "Nameless")
                .build()
                .expect("valid before we break it");
            d.name.clear();
            d
        }
        fn bus_layout(&self) -> BusLayout {
            self.0.bus_layout()
        }
        fn processor(&mut self) -> &mut dyn DauxProcessor {
            self.0.processor()
        }
        fn controller(&mut self) -> &mut dyn DauxController {
            self.0.controller()
        }
    }

    #[test]
    fn a_single_factory_enumerates_exactly_one_plug_in() {
        let factory = SingleFactory::<Counted>::new();
        assert_eq!(factory.plugin_count(), 1);
        assert_eq!(factory.descriptor(0).unwrap().id, Spy::descriptor().id);
        assert!(factory.descriptor(1).is_none());
        assert!(factory.descriptor(usize::MAX).is_none());
        assert_eq!(factory.descriptors().len(), 1);
        assert!(factory.contains(Spy::ID));
        assert!(!factory.contains("com.example.missing"));
    }

    #[test]
    fn a_single_factory_only_answers_to_its_own_id() {
        let factory = SingleFactory::<Counted>::new();
        for wrong in [
            "",
            "com.example.spy2",
            "COM.EXAMPLE.SPY",
            " com.example.spy",
        ] {
            let Err(err) = factory.create(wrong) else {
                panic!("{wrong:?} must not produce a plug-in")
            };
            assert_eq!(err.kind(), ErrorKind::NotFound, "{wrong:?} must not match");
            assert!(
                err.message().contains(Spy::ID),
                "the error names what it has"
            );
        }
        assert!(factory.create(Spy::ID).is_ok());
    }

    #[test]
    fn every_create_produces_a_new_plug_in() {
        let factory = SingleFactory::<Counted>::new();
        let a = factory.create(Spy::ID).unwrap();
        let b = factory.create(Spy::ID).unwrap();
        // Two distinct objects: hundreds of instances must coexist in one process (rule 6),
        // so a factory that handed out a shared singleton would be a serious bug.
        assert!(!std::ptr::addr_eq(
            std::ptr::from_ref(a.as_ref()),
            std::ptr::from_ref(b.as_ref())
        ));
        // Both are independently driveable.
        let mut first = crate::PluginInstance::new(a);
        let mut second = crate::PluginInstance::new(b);
        first.init().unwrap();
        second.init().unwrap();
        first.activate(&ProcessConfig::new(48_000.0, 64)).unwrap();
        assert_eq!(first.state(), crate::InstanceState::Active);
        assert_eq!(second.state(), crate::InstanceState::Inactive);
    }

    #[test]
    fn an_empty_registry_exports_nothing() {
        let registry = PluginRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert_eq!(registry.plugin_count(), 0);
        assert!(registry.descriptor(0).is_none());
        assert_eq!(registry.descriptors().len(), 0);
        let Err(err) = registry.create(Spy::ID) else {
            panic!("an empty registry has nothing to create")
        };
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn a_registry_enumerates_in_registration_order_and_dispatches_by_id() {
        let mut registry = PluginRegistry::new();
        registry.register::<Counted>().register::<Other>();

        assert_eq!(registry.len(), 2);
        assert_eq!(registry.ids().collect::<Vec<_>>(), [Spy::ID, Other::ID]);
        assert_eq!(registry.descriptor(0).unwrap().id, Spy::descriptor().id);
        assert_eq!(registry.descriptor(1).unwrap().id, Other::descriptor().id);
        assert!(registry.descriptor(2).is_none());

        // Dispatch reaches the right constructor, not merely *a* constructor.
        let spy = registry.create(Spy::ID).unwrap();
        assert_eq!(spy.bus_layout().inputs[0].layout.channel_count(), 2);
        let other = registry.create(Other::ID).unwrap();
        assert_eq!(other.bus_layout().inputs[0].layout.channel_count(), 1);
    }

    #[test]
    fn registering_the_same_id_twice_is_rejected_rather_than_shadowing() {
        let mut registry = PluginRegistry::new();
        registry.try_register::<Counted>().unwrap();

        let Err(err) = registry.try_register::<Counted>() else {
            panic!("a duplicate id must be rejected")
        };
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert!(err.message().contains(Spy::ID));
        assert!(
            err.message().contains("already registered"),
            "the message must say what went wrong: {err}"
        );

        assert_eq!(registry.len(), 1, "the duplicate must not be stored");
        assert_eq!(registry.plugin_count(), 1);
        // And the first registration is still the one that answers.
        assert!(registry.create(Spy::ID).is_ok());
    }

    /// A distinct type registering the *same permanent id* is the dangerous case: nothing
    /// about the Rust types collides, only the string a host saved in a project file.
    #[test]
    fn a_different_type_with_a_taken_id_is_rejected_too() {
        #[derive(Default)]
        struct Impostor(Other);
        impl DauxPlugin for Impostor {
            fn descriptor() -> PluginDescriptor {
                PluginDescriptor::builder(Spy::ID, "Impostor")
                    .build()
                    .expect("valid")
            }
            fn bus_layout(&self) -> BusLayout {
                self.0.bus_layout()
            }
            fn processor(&mut self) -> &mut dyn DauxProcessor {
                self.0.processor()
            }
            fn controller(&mut self) -> &mut dyn DauxController {
                self.0.controller()
            }
        }

        let mut registry = PluginRegistry::new();
        registry.try_register::<Counted>().unwrap();
        let Err(err) = registry.try_register::<Impostor>() else {
            panic!("a taken id must be rejected whatever type claims it")
        };
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry.descriptor(0).unwrap().name,
            "Spy",
            "the original must not have been replaced"
        );
    }

    #[test]
    fn an_invalid_descriptor_never_reaches_a_host() {
        let mut registry = PluginRegistry::new();
        let Err(err) = registry.try_register::<Nameless>() else {
            panic!("an invalid descriptor must be rejected")
        };
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert!(registry.is_empty());
    }

    #[test]
    #[should_panic(expected = "already registered")]
    fn the_chaining_form_panics_on_a_duplicate() {
        let mut registry = PluginRegistry::new();
        registry.register::<Counted>().register::<Counted>();
    }

    #[test]
    #[should_panic(expected = "PluginRegistry::register")]
    fn the_chaining_form_panics_on_an_invalid_descriptor() {
        let _ = PluginRegistry::new().with::<Nameless>();
    }

    #[test]
    fn the_by_value_builder_produces_the_same_registry() {
        let registry = PluginRegistry::new().with::<Counted>().with::<Other>();
        assert_eq!(registry.ids().collect::<Vec<_>>(), [Spy::ID, Other::ID]);
        assert!(registry.contains(Other::ID));
    }

    #[test]
    fn an_unknown_id_is_not_found_however_many_are_registered() {
        let registry = PluginRegistry::new().with::<Counted>().with::<Other>();
        for wrong in ["", "com.example", "com.example.spy.extra"] {
            let Err(err) = registry.create(wrong) else {
                panic!("{wrong:?} must not produce a plug-in")
            };
            assert_eq!(err.kind(), ErrorKind::NotFound, "{wrong:?}");
        }
    }

    /// A factory is handed to the host and may be consulted from several threads
    /// (`DauxFactory: Send + Sync + 'static`), so both implementations must really be.
    #[test]
    fn both_factories_are_shareable() {
        const fn assert_factory<F: DauxFactory>() {}
        assert_factory::<SingleFactory<Counted>>();
        assert_factory::<PluginRegistry>();

        let registry = std::sync::Arc::new(PluginRegistry::new().with::<Counted>());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let registry = registry.clone();
                std::thread::spawn(move || registry.create(Spy::ID).is_ok())
            })
            .collect();
        for h in handles {
            assert!(h.join().unwrap());
        }
    }

    #[test]
    fn a_single_factory_is_copyable_and_describes_itself() {
        let factory = SingleFactory::<Counted>::new();
        let copy = factory;
        assert_eq!(copy.plugin_count(), factory.plugin_count());
        assert!(format!("{factory:?}").contains(Spy::ID));
        assert!(format!("{:?}", SingleFactory::<Other>::default()).contains(Other::ID));
    }

    #[test]
    fn a_registry_describes_itself_by_id() {
        let registry = PluginRegistry::new().with::<Counted>().with::<Other>();
        let debug = format!("{registry:?}");
        assert!(debug.contains(Spy::ID), "{debug}");
        assert!(debug.contains(Other::ID), "{debug}");
    }
}
