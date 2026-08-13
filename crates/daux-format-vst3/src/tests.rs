//! The suite that drives the adapter the way a DAW does: through raw C pointers.
//!
//! Every test here goes in at an exported entry point and reads the answer as a `TResult` or
//! a written-out `#[repr(C)]` struct. Nothing calls a Rust method on the objects under test,
//! because the vtable layout, the reference counting, the null handling and the panic
//! boundary are only real at the ABI — a Rust-level test would pass with all four broken.

use core::ffi::c_void;
use core::sync::atomic::Ordering;
use std::sync::Arc;

use daux_plugin_api::{DauxFactory, DauxPlugin, DauxResult, PluginDescriptor, SingleFactory};

use crate::api::{
    self, AudioBusBuffers, BusInfo, IAudioProcessorVtbl, IComponentVtbl, IConnectionPointVtbl,
    IEditControllerVtbl, IPlugViewVtbl, IPluginFactoryVtbl, PClassInfo, PClassInfo2, PClassInfoW,
    PFactoryInfo, ParameterInfo, ProcessData, ProcessSetup, ViewRect, bus_direction, media_type,
    sample_size,
};
use crate::com::{TUid, result};
use crate::entry;
use crate::factory::Vst3Factory;
use crate::testkit::{
    Counts, FakeComponentHandler, FakeEventList, FakeParamQueue, FakeParameterChanges, HandlerCall,
    SpyPlugin, VecStream,
};

// ---------------------------------------------------------------------------------------
// Thin call helpers: the same indirection a host performs, written once.
// ---------------------------------------------------------------------------------------

/// The factory vtable behind a `*mut c_void`.
///
/// # Safety
///
/// `p` must be a live `IPluginFactory`.
unsafe fn factory_vtbl(p: *mut c_void) -> &'static IPluginFactoryVtbl {
    // SAFETY: the caller promises a live factory, whose first word is its vtable pointer.
    unsafe { &**p.cast::<*const IPluginFactoryVtbl>() }
}

/// # Safety
///
/// `p` must be a live `IComponent`.
unsafe fn component_vtbl(p: *mut c_void) -> &'static IComponentVtbl {
    // SAFETY: as `factory_vtbl`.
    unsafe { &**p.cast::<*const IComponentVtbl>() }
}

/// # Safety
///
/// `p` must be a live `IAudioProcessor`.
unsafe fn audio_vtbl(p: *mut c_void) -> &'static IAudioProcessorVtbl {
    // SAFETY: as `factory_vtbl`.
    unsafe { &**p.cast::<*const IAudioProcessorVtbl>() }
}

/// # Safety
///
/// `p` must be a live `IEditController`.
unsafe fn controller_vtbl(p: *mut c_void) -> &'static IEditControllerVtbl {
    // SAFETY: as `factory_vtbl`.
    unsafe { &**p.cast::<*const IEditControllerVtbl>() }
}

/// # Safety
///
/// `p` must be a live `IPlugView`.
unsafe fn view_vtbl(p: *mut c_void) -> &'static IPlugViewVtbl {
    // SAFETY: as `factory_vtbl`.
    unsafe { &**p.cast::<*const IPlugViewVtbl>() }
}

/// Asks any COM object for an interface, exactly as a host would.
///
/// # Safety
///
/// `object` must be a live COM object.
unsafe fn query(object: *mut c_void, iid: &TUid) -> *mut c_void {
    let mut out: *mut c_void = core::ptr::null_mut();
    // SAFETY: the caller promises a live object; `out` is a live local.
    let status = unsafe {
        let vtbl = &**object.cast::<*const crate::com::FUnknownVtbl>();
        (vtbl.query_interface)(object, core::ptr::from_ref(iid), &raw mut out)
    };
    if result::is_ok(status) {
        out
    } else {
        core::ptr::null_mut()
    }
}

/// # Safety
///
/// `object` must be a live COM object the caller owns a reference to.
unsafe fn release(object: *mut c_void) -> u32 {
    // SAFETY: the caller promises a live object.
    unsafe { crate::com::release(object) }
}

/// # Safety
///
/// `object` must be a live COM object.
unsafe fn add_ref(object: *mut c_void) -> u32 {
    // SAFETY: the caller promises a live object.
    unsafe { crate::com::add_ref(object) }
}

// ---------------------------------------------------------------------------------------
// A module under test
// ---------------------------------------------------------------------------------------

/// The factory the tests export, standing in for `export_entry!(SpyFactory)`.
#[derive(Default)]
struct SpyFactory;

impl DauxFactory for SpyFactory {
    fn plugin_count(&self) -> usize {
        1
    }

    fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
        (index == 0).then(SpyPlugin::descriptor)
    }

    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
        if id == SpyPlugin::ID {
            Ok(Box::new(SpyPlugin::new(Arc::new(Counts::default()))))
        } else {
            Err(daux_plugin_api::ErrorKind::NotFound.error("no such plug-in"))
        }
    }
}

/// A factory that hands out plug-ins sharing one [`Counts`], so a test can watch them.
struct WatchedFactory {
    counts: Arc<Counts>,
    headless: bool,
}

impl DauxFactory for WatchedFactory {
    fn plugin_count(&self) -> usize {
        1
    }

    fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
        (index == 0).then(SpyPlugin::descriptor)
    }

    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
        if id != SpyPlugin::ID {
            return Err(daux_plugin_api::ErrorKind::NotFound.error("no such plug-in"));
        }
        Ok(if self.headless {
            Box::new(SpyPlugin::headless(Arc::clone(&self.counts)))
        } else {
            Box::new(SpyPlugin::new(Arc::clone(&self.counts)))
        })
    }
}

/// One module, one instance and the counters behind it.
struct Harness {
    factory: *mut c_void,
    component: *mut c_void,
    counts: Arc<Counts>,
}

impl Harness {
    /// Builds a module and creates one instance's `IComponent`, as a host does.
    fn new() -> Self {
        Self::with(false)
    }

    /// The same, for a plug-in with no editor.
    fn headless() -> Self {
        Self::with(true)
    }

    fn with(headless: bool) -> Self {
        let counts = Arc::new(Counts::default());
        let factory = Vst3Factory::create(Box::new(WatchedFactory {
            counts: Arc::clone(&counts),
            headless,
        }));
        let cid = crate::cid::class_id(SpyPlugin::ID);
        let mut component: *mut c_void = core::ptr::null_mut();
        // SAFETY: `factory` is live; `cid` and the iid are live locals of the right size, and
        // `component` is a live out-parameter.
        let status = unsafe {
            (factory_vtbl(factory).create_instance)(
                factory,
                core::ptr::from_ref(&cid).cast(),
                core::ptr::from_ref(&api::ICOMPONENT_IID).cast(),
                &raw mut component,
            )
        };
        assert_eq!(status, result::OK, "the factory must create its own class");
        assert!(!component.is_null());
        Self {
            factory,
            component,
            counts,
        }
    }

    /// Runs the whole VST3 start-up sequence, in the order a host uses.
    fn start(&self, sample_rate: f64, block: i32) {
        // SAFETY: `self.component` is live for the harness's lifetime.
        unsafe {
            assert_eq!(
                (component_vtbl(self.component).initialize)(self.component, core::ptr::null_mut()),
                result::OK
            );
            let audio = query(self.component, &api::IAUDIO_PROCESSOR_IID);
            assert!(!audio.is_null());
            let mut setup = ProcessSetup {
                process_mode: api::process_mode::REALTIME,
                symbolic_sample_size: sample_size::SAMPLE32,
                max_samples_per_block: block,
                sample_rate,
            };
            assert_eq!(
                (audio_vtbl(audio).setup_processing)(audio, &raw mut setup),
                result::OK
            );
            assert_eq!(
                (component_vtbl(self.component).set_active)(self.component, 1),
                result::OK
            );
            assert_eq!((audio_vtbl(audio).set_processing)(audio, 1), result::OK);
            release(audio);
        }
    }

    /// The instance's `IAudioProcessor`, with a reference the caller owns.
    fn audio(&self) -> *mut c_void {
        // SAFETY: `self.component` is live.
        unsafe { query(self.component, &api::IAUDIO_PROCESSOR_IID) }
    }

    /// The instance's `IEditController`, with a reference the caller owns.
    fn controller(&self) -> *mut c_void {
        // SAFETY: `self.component` is live.
        unsafe { query(self.component, &api::IEDIT_CONTROLLER_IID) }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // SAFETY: the harness owns one reference to each.
        unsafe {
            release(self.component);
            release(self.factory);
        }
    }
}

/// Silences panic output for a test that deliberately causes one.
fn quietly<R>(f: impl FnOnce() -> R) -> R {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = f();
    std::panic::set_hook(previous);
    out
}

/// A block of `frames` stereo samples in and out, with the pointer arrays a host builds.
struct Block {
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    input_ptrs: Vec<*mut c_void>,
    output_ptrs: Vec<*mut c_void>,
    buses: Vec<AudioBusBuffers>,
    frames: usize,
}

impl Block {
    fn new(frames: usize, value: f32) -> Self {
        Self {
            input: vec![vec![value; frames]; 2],
            output: vec![vec![0.0; frames]; 2],
            input_ptrs: Vec::new(),
            output_ptrs: Vec::new(),
            buses: Vec::new(),
            frames,
        }
    }

    /// Builds the `ProcessData` a host would pass, with the pointer arrays it owns.
    fn data(&mut self) -> ProcessData {
        self.input_ptrs = self
            .input
            .iter_mut()
            .map(|c| c.as_mut_ptr().cast::<c_void>())
            .collect();
        self.output_ptrs = self
            .output
            .iter_mut()
            .map(|c| c.as_mut_ptr().cast::<c_void>())
            .collect();
        self.buses = vec![
            AudioBusBuffers {
                num_channels: 2,
                silence_flags: 0,
                channel_buffers: self.input_ptrs.as_mut_ptr(),
            },
            AudioBusBuffers {
                num_channels: 2,
                silence_flags: u64::MAX,
                channel_buffers: self.output_ptrs.as_mut_ptr(),
            },
        ];
        ProcessData {
            process_mode: api::process_mode::REALTIME,
            symbolic_sample_size: sample_size::SAMPLE32,
            num_samples: i32::try_from(self.frames).unwrap(),
            num_inputs: 1,
            num_outputs: 1,
            inputs: &raw mut self.buses[0],
            outputs: &raw mut self.buses[1],
            input_parameter_changes: core::ptr::null_mut(),
            output_parameter_changes: core::ptr::null_mut(),
            input_events: core::ptr::null_mut(),
            output_events: core::ptr::null_mut(),
            process_context: core::ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// The factory
// ---------------------------------------------------------------------------------------

#[test]
fn the_entry_point_hands_out_a_factory_that_answers_all_three_interfaces() {
    let factory = entry::get_plugin_factory::<SingleFactory<SpyPlugin>>();
    assert!(!factory.is_null(), "GetPluginFactory must not return null");

    // SAFETY: `factory` is live and owns one reference.
    unsafe {
        for iid in [
            api::IFUNKNOWN_IID,
            api::IPLUGIN_FACTORY_IID,
            api::IPLUGIN_FACTORY2_IID,
            api::IPLUGIN_FACTORY3_IID,
        ] {
            let got = query(factory, &iid);
            assert!(!got.is_null(), "the factory must answer every version");
            assert_eq!(got, factory, "all three are one object");
            release(got);
        }
        // …and refuses one it does not implement.
        assert!(query(factory, &api::ICOMPONENT_IID).is_null());
        assert_eq!(release(factory), 0);
    }
}

#[test]
fn every_call_to_the_entry_point_produces_a_separate_factory() {
    let a = entry::get_plugin_factory::<SingleFactory<SpyPlugin>>();
    let b = entry::get_plugin_factory::<SingleFactory<SpyPlugin>>();
    assert_ne!(a, b, "a singleton would be global mutable state");
    // SAFETY: both are live and each owns one reference.
    unsafe {
        assert_eq!(release(a), 0);
        assert_eq!(release(b), 0);
    }
}

#[test]
fn the_class_list_describes_the_plug_in_a_host_will_show() {
    let factory = Vst3Factory::create(Box::new(SpyFactory));
    // SAFETY: `factory` is live for the rest of the test.
    unsafe {
        let vtbl = factory_vtbl(factory);
        assert_eq!((vtbl.count_classes)(factory), 1);

        let mut info = core::mem::zeroed::<PClassInfo>();
        assert_eq!((vtbl.get_class_info)(factory, 0, &raw mut info), result::OK);
        assert_eq!(info.cid, crate::cid::class_id(SpyPlugin::ID));
        assert_eq!(info.cardinality, api::K_MANY_INSTANCES);
        assert_eq!(cstr(&info.category), "Audio Module Class");
        assert_eq!(cstr(&info.name), "Spy");

        let mut info2 = core::mem::zeroed::<PClassInfo2>();
        assert_eq!(
            (vtbl.get_class_info2)(factory, 0, &raw mut info2),
            result::OK
        );
        assert_eq!(cstr(&info2.subcategories), "Fx|Filter");
        assert_eq!(cstr(&info2.vendor), "Example");
        assert_eq!(cstr(&info2.version), "2.3.4");
        assert!(cstr(&info2.sdk_version).starts_with("VST 3"));
        assert_eq!(
            info2.class_flags, 0,
            "never kDistributable: the two halves are one object"
        );

        let mut info_w = core::mem::zeroed::<PClassInfoW>();
        assert_eq!(
            (vtbl.get_class_info_unicode)(factory, 0, &raw mut info_w),
            result::OK
        );
        assert_eq!(utf16(&info_w.name), "Spy");
        assert_eq!(utf16(&info_w.vendor), "Example");
        assert_eq!(info_w.cid, info.cid, "one class, one id, in both encodings");

        let mut factory_info = core::mem::zeroed::<PFactoryInfo>();
        assert_eq!(
            (vtbl.get_factory_info)(factory, &raw mut factory_info),
            result::OK
        );
        assert_eq!(cstr(&factory_info.vendor), "Example");
        assert_eq!(cstr(&factory_info.url), "https://example.com");
        assert!(factory_info.flags & api::factory_flags::UNICODE != 0);

        release(factory);
    }
}

#[test]
fn the_factory_refuses_indices_and_pointers_a_broken_host_might_pass() {
    let factory = Vst3Factory::create(Box::new(SpyFactory));
    // SAFETY: `factory` is live; every pointer below is either live or deliberately null.
    unsafe {
        let vtbl = factory_vtbl(factory);
        let mut info = core::mem::zeroed::<PClassInfo>();
        assert_ne!(
            (vtbl.get_class_info)(factory, -1, &raw mut info),
            result::OK
        );
        assert_ne!((vtbl.get_class_info)(factory, 1, &raw mut info), result::OK);
        assert_ne!(
            (vtbl.get_class_info)(factory, i32::MIN, &raw mut info),
            result::OK
        );
        assert_eq!(
            (vtbl.get_class_info)(factory, 0, core::ptr::null_mut()),
            result::INVALID_ARGUMENT
        );
        assert_eq!(
            (vtbl.get_factory_info)(factory, core::ptr::null_mut()),
            result::INVALID_ARGUMENT
        );

        // A null `this` must not be dereferenced either — hosts have shipped that bug.
        assert_eq!((vtbl.count_classes)(core::ptr::null_mut()), 0);
        assert_eq!((vtbl.add_ref)(core::ptr::null_mut()), 0);
        assert_eq!((vtbl.release)(core::ptr::null_mut()), 0);

        release(factory);
    }
}

#[test]
fn creating_an_instance_of_an_unknown_class_fails_without_leaking() {
    let counts = Arc::new(Counts::default());
    let factory = Vst3Factory::create(Box::new(WatchedFactory {
        counts: Arc::clone(&counts),
        headless: false,
    }));
    let wrong = crate::cid::class_id("com.example.not-in-this-module");
    let mut out: *mut c_void = core::ptr::without_provenance_mut(0xDEAD);
    // SAFETY: `factory` is live, `wrong` is a live local, `out` is a live out-parameter.
    unsafe {
        let status = (factory_vtbl(factory).create_instance)(
            factory,
            core::ptr::from_ref(&wrong).cast(),
            core::ptr::from_ref(&api::ICOMPONENT_IID).cast(),
            &raw mut out,
        );
        assert_ne!(status, result::OK);
        assert!(
            out.is_null(),
            "a failed creation must clear the out pointer"
        );
        assert!(
            !counts.dropped.load(Ordering::Acquire),
            "no plug-in should have been built at all"
        );

        // Null pointers, too.
        assert_eq!(
            (factory_vtbl(factory).create_instance)(
                factory,
                core::ptr::null(),
                core::ptr::from_ref(&api::ICOMPONENT_IID).cast(),
                &raw mut out,
            ),
            result::INVALID_ARGUMENT
        );
        assert_eq!(
            (factory_vtbl(factory).create_instance)(
                factory,
                core::ptr::from_ref(&wrong).cast(),
                core::ptr::from_ref(&api::ICOMPONENT_IID).cast(),
                core::ptr::null_mut(),
            ),
            result::INVALID_ARGUMENT
        );
        release(factory);
    }
}

#[test]
fn asking_for_an_interface_the_instance_does_not_have_frees_it_rather_than_leaking_it() {
    let counts = Arc::new(Counts::default());
    let factory = Vst3Factory::create(Box::new(WatchedFactory {
        counts: Arc::clone(&counts),
        headless: false,
    }));
    let cid = crate::cid::class_id(SpyPlugin::ID);
    let mut out: *mut c_void = core::ptr::null_mut();
    // SAFETY: `factory`, `cid` and `out` are all live.
    unsafe {
        let status = (factory_vtbl(factory).create_instance)(
            factory,
            core::ptr::from_ref(&cid).cast(),
            // A real interface id, but not one the component implements.
            core::ptr::from_ref(&api::IBSTREAM_IID).cast(),
            &raw mut out,
        );
        assert_ne!(status, result::OK);
        assert!(out.is_null());
        assert!(
            counts.dropped.load(Ordering::Acquire),
            "the instance was built and must have been destroyed again"
        );
        release(factory);
    }
}

// ---------------------------------------------------------------------------------------
// COM identity and reference counting
// ---------------------------------------------------------------------------------------

#[test]
fn every_head_leads_back_to_one_object_and_one_identity() {
    let harness = Harness::new();
    // SAFETY: the component is live for the harness's lifetime.
    unsafe {
        let audio = query(harness.component, &api::IAUDIO_PROCESSOR_IID);
        let controller = query(harness.component, &api::IEDIT_CONTROLLER_IID);
        let connection = query(harness.component, &api::ICONNECTION_POINT_IID);
        assert!(!audio.is_null() && !controller.is_null() && !connection.is_null());
        // The heads are distinct pointers — they have to be, they have different vtables.
        assert_ne!(audio, harness.component);
        assert_ne!(controller, audio);
        assert_ne!(connection, controller);

        // …but `FUnknown` from any of them is the same pointer, which is what tells a host
        // "these four interfaces are one object".
        let identities: Vec<*mut c_void> = [harness.component, audio, controller, connection]
            .into_iter()
            .map(|head| {
                let unknown = query(head, &api::IFUNKNOWN_IID);
                assert!(!unknown.is_null());
                release(unknown);
                unknown
            })
            .collect();
        assert!(
            identities.windows(2).all(|w| w[0] == w[1]),
            "COM identity is broken: {identities:?}"
        );

        // Every head can reach every other head, and lands on the same pointer as the
        // original query did.
        let back_to_component = query(audio, &api::ICOMPONENT_IID);
        assert_eq!(back_to_component, harness.component);
        release(back_to_component);
        let back_to_audio = query(controller, &api::IAUDIO_PROCESSOR_IID);
        assert_eq!(back_to_audio, audio);
        release(back_to_audio);

        // Exactly the three references this test took, given back.
        release(audio);
        release(controller);
        release(connection);
    }
    // …leaving the harness's own reference, which its `Drop` releases.
}

#[test]
fn an_instance_lives_exactly_as_long_as_its_references() {
    let counts = Arc::new(Counts::default());
    let factory = Vst3Factory::create(Box::new(WatchedFactory {
        counts: Arc::clone(&counts),
        headless: false,
    }));
    let cid = crate::cid::class_id(SpyPlugin::ID);
    let mut component: *mut c_void = core::ptr::null_mut();
    // SAFETY: every pointer below is live.
    unsafe {
        (factory_vtbl(factory).create_instance)(
            factory,
            core::ptr::from_ref(&cid).cast(),
            core::ptr::from_ref(&api::ICOMPONENT_IID).cast(),
            &raw mut component,
        );
        assert!(!component.is_null());
        assert!(!counts.dropped.load(Ordering::Acquire));

        assert_eq!(add_ref(component), 2);
        assert_eq!(add_ref(component), 3);
        // A queried interface holds a reference of its own.
        let audio = query(component, &api::IAUDIO_PROCESSOR_IID);
        assert_eq!(release(audio), 3);
        assert_eq!(release(component), 2);
        assert_eq!(release(component), 1);
        assert!(
            !counts.dropped.load(Ordering::Acquire),
            "one reference is still outstanding"
        );
        assert_eq!(release(component), 0);
        assert!(
            counts.dropped.load(Ordering::Acquire),
            "the last release must destroy the plug-in"
        );
        release(factory);
    }
}

// ---------------------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------------------

#[test]
fn the_hosts_start_up_sequence_maps_onto_the_daux_lifecycle_in_order() {
    let harness = Harness::new();
    let counts = &harness.counts;
    // SAFETY: the component is live.
    unsafe {
        let component = harness.component;
        let audio = harness.audio();

        assert_eq!(
            (component_vtbl(component).initialize)(component, core::ptr::null_mut()),
            result::OK
        );
        assert_eq!(
            Counts::get(&counts.prepares),
            0,
            "initialize is not prepare"
        );

        let mut setup = ProcessSetup {
            process_mode: api::process_mode::REALTIME,
            symbolic_sample_size: sample_size::SAMPLE32,
            max_samples_per_block: 256,
            sample_rate: 44_100.0,
        };
        assert_eq!(
            (audio_vtbl(audio).setup_processing)(audio, &raw mut setup),
            result::OK
        );
        assert_eq!(
            Counts::get(&counts.prepares),
            0,
            "setupProcessing only records the configuration"
        );

        assert_eq!(
            (component_vtbl(component).set_active)(component, 1),
            result::OK
        );
        assert_eq!(Counts::get(&counts.prepares), 1, "setActive is prepare");
        assert_eq!(Counts::get(&counts.activates), 0);

        assert_eq!((audio_vtbl(audio).set_processing)(audio, 1), result::OK);
        assert_eq!(
            Counts::get(&counts.activates),
            1,
            "setProcessing is activate"
        );

        assert_eq!((audio_vtbl(audio).set_processing)(audio, 0), result::OK);
        assert_eq!(Counts::get(&counts.deactivates), 1);
        assert_eq!(
            (component_vtbl(component).set_active)(component, 0),
            result::OK
        );
        assert_eq!((component_vtbl(component).terminate)(component), result::OK);

        // The whole cycle again on the same object, as a host does when the rate changes.
        assert_eq!(
            (component_vtbl(component).initialize)(component, core::ptr::null_mut()),
            result::NOT_INITIALIZED,
            "initialize is not repeatable"
        );
        release(audio);
    }
}

#[test]
fn a_host_that_initialises_both_halves_gets_a_success_from_the_second_one() {
    let harness = Harness::new();
    // SAFETY: every pointer is live.
    unsafe {
        let component = harness.component;
        let controller = harness.controller();
        assert_eq!(
            (component_vtbl(component).initialize)(component, core::ptr::null_mut()),
            result::OK
        );
        // A host that treats the two halves as separate objects initialises both. For a
        // single-component effect the second call has nothing to do, and refusing it makes
        // the host give up on the plug-in.
        assert_eq!(
            (controller_vtbl(controller).initialize)(controller, core::ptr::null_mut()),
            result::OK
        );
        assert_eq!(
            Counts::get(&harness.counts.prepares),
            0,
            "neither initialize may reach the DSP"
        );
        assert_eq!(
            (controller_vtbl(controller).get_parameter_count)(controller),
            3,
            "the parameter mirror must have been built exactly once"
        );
        assert_eq!(
            (controller_vtbl(controller).terminate)(controller),
            result::OK
        );
        release(controller);
    }
}

#[test]
fn processing_before_the_host_has_armed_it_is_refused_rather_than_obeyed() {
    let harness = Harness::new();
    let mut block = Block::new(64, 0.5);
    // SAFETY: every pointer is live.
    unsafe {
        let audio = harness.audio();
        let mut data = block.data();

        // Not initialised, not active, not processing.
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut data),
            result::NOT_INITIALIZED
        );
        assert_eq!(Counts::get(&harness.counts.processes), 0);

        // `setProcessing` before `setActive` is refused too.
        assert_ne!((audio_vtbl(audio).set_processing)(audio, 1), result::OK);
        assert_eq!(Counts::get(&harness.counts.activates), 0);
        release(audio);
    }
}

#[test]
fn a_block_longer_than_the_host_promised_is_refused() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    let mut block = Block::new(65, 0.5);
    // SAFETY: every pointer is live.
    unsafe {
        let audio = harness.audio();
        let mut data = block.data();
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut data),
            result::INVALID_ARGUMENT,
            "the plug-in sized its buffers for 64 frames"
        );
        assert_eq!(Counts::get(&harness.counts.processes), 0);

        // Exactly the maximum is fine.
        let mut ok = Block::new(64, 0.5);
        let mut data = ok.data();
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut data),
            result::OK
        );
        assert_eq!(Counts::get(&harness.counts.processes), 1);
        release(audio);
    }
}

#[test]
fn a_block_of_audio_reaches_the_plug_in_and_comes_back_changed() {
    let harness = Harness::new();
    harness.start(48_000.0, 128);
    let mut block = Block::new(128, 0.5);
    // SAFETY: every pointer is live.
    unsafe {
        let audio = harness.audio();
        let mut data = block.data();
        // The host claims both output channels are silent; the plug-in writes real samples,
        // so the adapter must clear the claim or the host will discard the audio.
        assert_eq!(data.num_outputs, 1);
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut data),
            result::OK
        );
        release(audio);
    }
    assert_eq!(Counts::get(&harness.counts.processes), 1);
    assert_eq!(block.buses[1].silence_flags, 0, "stale silence flags");
    // The spy applies its gain parameter, which defaults to 0 dB.
    for channel in &block.output {
        assert!(
            channel.iter().all(|&s| (s - 0.5).abs() < 1e-6),
            "wrong audio"
        );
    }
}

#[test]
fn null_and_nonsense_process_data_is_refused_without_a_crash() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    // SAFETY: `audio` is live; the data pointers below are deliberately degenerate.
    unsafe {
        let audio = harness.audio();
        assert_eq!(
            (audio_vtbl(audio).process)(audio, core::ptr::null_mut()),
            result::INVALID_ARGUMENT
        );

        // A block with no buses at all: legal, and the plug-in must survive it.
        let mut empty = ProcessData {
            process_mode: api::process_mode::REALTIME,
            symbolic_sample_size: sample_size::SAMPLE32,
            num_samples: 32,
            num_inputs: 0,
            num_outputs: 0,
            inputs: core::ptr::null_mut(),
            outputs: core::ptr::null_mut(),
            input_parameter_changes: core::ptr::null_mut(),
            output_parameter_changes: core::ptr::null_mut(),
            input_events: core::ptr::null_mut(),
            output_events: core::ptr::null_mut(),
            process_context: core::ptr::null_mut(),
        };
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut empty),
            result::OK
        );

        // A negative frame count.
        let mut negative = ProcessData {
            num_samples: -1,
            ..empty
        };
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut negative),
            result::INVALID_ARGUMENT
        );
        release(audio);
    }
}

#[test]
fn a_bus_the_host_has_switched_off_arrives_as_a_channel_less_bus_of_the_right_length() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    let mut block = Block::new(64, 0.5);
    // SAFETY: every pointer is live.
    unsafe {
        let audio = harness.audio();
        let mut data = block.data();
        // A deactivated bus: still present, still part of a 64-frame block, but with no
        // channels and no pointer array.
        (*data.inputs).num_channels = 0;
        (*data.inputs).channel_buffers = core::ptr::null_mut();
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut data),
            result::OK,
            "a switched-off bus must not fail the block"
        );
        release(audio);
    }
    assert_eq!(Counts::get(&harness.counts.processes), 1);
    // With no input to read, the spy silences its output rather than reading past the end.
    assert!(block.output[0].iter().all(|&s| s == 0.0));
}

// ---------------------------------------------------------------------------------------
// Buses
// ---------------------------------------------------------------------------------------

#[test]
fn the_bus_topology_a_host_sees_is_the_one_the_plug_in_declared() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    // SAFETY: the component is live.
    unsafe {
        let component = harness.component;
        let vtbl = component_vtbl(component);
        assert_eq!(
            (vtbl.get_bus_count)(component, media_type::AUDIO, bus_direction::INPUT),
            1
        );
        assert_eq!(
            (vtbl.get_bus_count)(component, media_type::AUDIO, bus_direction::OUTPUT),
            1
        );
        // The spy is an instrument-shaped event layout: one input port, no output port.
        assert_eq!(
            (vtbl.get_bus_count)(component, media_type::EVENT, bus_direction::INPUT),
            1
        );
        assert_eq!(
            (vtbl.get_bus_count)(component, media_type::EVENT, bus_direction::OUTPUT),
            0
        );
        assert_eq!((vtbl.get_bus_count)(component, 99, bus_direction::INPUT), 0);

        let mut info = core::mem::zeroed::<BusInfo>();
        assert_eq!(
            (vtbl.get_bus_info)(
                component,
                media_type::AUDIO,
                bus_direction::INPUT,
                0,
                &raw mut info
            ),
            result::OK
        );
        assert_eq!(info.channel_count, 2);
        assert_eq!(info.bus_type, api::bus_type::MAIN);
        assert!(!utf16(&info.name).is_empty());

        // Out of range and null are refused.
        assert_ne!(
            (vtbl.get_bus_info)(
                component,
                media_type::AUDIO,
                bus_direction::INPUT,
                7,
                &raw mut info
            ),
            result::OK
        );
        assert_eq!(
            (vtbl.get_bus_info)(
                component,
                media_type::AUDIO,
                bus_direction::INPUT,
                0,
                core::ptr::null_mut()
            ),
            result::INVALID_ARGUMENT
        );

        // A single-component effect tells the host to query the controller from it.
        let mut cid = [0u8; 16];
        assert_eq!(
            (vtbl.get_controller_class_id)(component, &raw mut cid),
            result::NOT_IMPLEMENTED
        );
    }
}

#[test]
fn speaker_arrangements_are_accepted_only_when_the_channel_counts_line_up() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    // SAFETY: `audio` is live and every array below is a live local.
    unsafe {
        let audio = harness.audio();
        let vtbl = audio_vtbl(audio);

        let stereo = crate::mapping::speaker_arrangement(daux_plugin_api::ChannelLayout::Stereo);
        let mono = crate::mapping::speaker_arrangement(daux_plugin_api::ChannelLayout::Mono);

        let mut ins = [stereo];
        let mut outs = [stereo];
        assert_eq!(
            (vtbl.set_bus_arrangements)(audio, ins.as_mut_ptr(), 1, outs.as_mut_ptr(), 1),
            result::TRUE
        );

        let mut wrong = [mono];
        assert_eq!(
            (vtbl.set_bus_arrangements)(audio, wrong.as_mut_ptr(), 1, outs.as_mut_ptr(), 1),
            result::FALSE,
            "a mono input on a stereo plug-in must be refused, not silently accepted"
        );
        assert_eq!(
            (vtbl.set_bus_arrangements)(audio, ins.as_mut_ptr(), 2, outs.as_mut_ptr(), 1),
            result::FALSE,
            "the wrong number of buses must be refused"
        );

        let mut got = 0u64;
        assert_eq!(
            (vtbl.get_bus_arrangement)(audio, bus_direction::OUTPUT, 0, &raw mut got),
            result::OK
        );
        assert_eq!(got, stereo);
        assert_ne!(
            (vtbl.get_bus_arrangement)(audio, bus_direction::OUTPUT, 5, &raw mut got),
            result::OK
        );

        assert_eq!(
            (vtbl.can_process_sample_size)(audio, sample_size::SAMPLE32),
            result::OK
        );
        assert_eq!(
            (vtbl.can_process_sample_size)(audio, sample_size::SAMPLE64),
            result::FALSE,
            "the spy declares f32 only"
        );
        assert_eq!((vtbl.can_process_sample_size)(audio, 99), result::FALSE);

        assert_eq!((vtbl.get_latency_samples)(audio), 64);
        assert_eq!((vtbl.get_tail_samples)(audio), 128);
        release(audio);
    }
}

// ---------------------------------------------------------------------------------------
// Parameters: the normalised/plain boundary
// ---------------------------------------------------------------------------------------

#[test]
fn the_parameter_list_a_host_sees_carries_the_daux_ids_verbatim() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    // SAFETY: `controller` is live.
    unsafe {
        let controller = harness.controller();
        let vtbl = controller_vtbl(controller);
        assert_eq!((vtbl.get_parameter_count)(controller), 3);

        let mut info = core::mem::zeroed::<ParameterInfo>();
        assert_eq!(
            (vtbl.get_parameter_info)(controller, 0, &raw mut info),
            result::OK
        );
        assert_eq!(info.id, 1, "the VST3 id is the permanent DAUx id");
        assert_eq!(utf16(&info.title), "Gain");
        assert_eq!(utf16(&info.units), "dB");
        assert_eq!(info.step_count, 0);
        assert!(info.flags & api::param_flags::CAN_AUTOMATE != 0);
        // 0 dB on a -60..+12 range.
        assert!((info.default_normalized_value - (60.0 / 72.0)).abs() < 1e-12);

        assert_eq!(
            (vtbl.get_parameter_info)(controller, 2, &raw mut info),
            result::OK
        );
        assert_eq!(info.id, 3);
        assert_eq!(info.step_count, 15, "1..=16 is fifteen intervals");
        assert!(info.flags & api::param_flags::IS_LIST != 0);

        assert_ne!(
            (vtbl.get_parameter_info)(controller, 3, &raw mut info),
            result::OK
        );
        assert_ne!(
            (vtbl.get_parameter_info)(controller, -1, &raw mut info),
            result::OK
        );
        release(controller);
    }
}

#[test]
fn normalised_and_plain_conversions_follow_the_parameters_own_curve() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    // SAFETY: `controller` is live.
    unsafe {
        let controller = harness.controller();
        let vtbl = controller_vtbl(controller);

        // The logarithmic one. A linear reading of the same normalised value would answer
        // 10 010 Hz, which is the bug this whole adapter is shaped around avoiding.
        let mid = (vtbl.normalized_param_to_plain)(controller, 2, 0.5);
        assert!(
            (mid - 632.455_532_033_675_9).abs() < 1e-6,
            "half a knob's travel on 20..20k is the geometric mean, got {mid}"
        );
        assert!((mid - 10_010.0).abs() > 1000.0);
        assert!(((vtbl.plain_param_to_normalized)(controller, 2, mid) - 0.5).abs() < 1e-9);
        assert!((vtbl.normalized_param_to_plain)(controller, 2, 0.0) - 20.0 < 1e-9);
        assert!(((vtbl.normalized_param_to_plain)(controller, 2, 1.0) - 20_000.0).abs() < 1e-6);

        // The linear one, for contrast.
        assert!(((vtbl.normalized_param_to_plain)(controller, 1, 0.5) + 24.0).abs() < 1e-9);

        // An unknown id answers 0 rather than reading past the table.
        assert_eq!((vtbl.normalized_param_to_plain)(controller, 999, 0.5), 0.0);
        assert_eq!((vtbl.plain_param_to_normalized)(controller, 999, 0.5), 0.0);
        release(controller);
    }
}

#[test]
fn automation_arrives_at_the_plug_in_as_a_plain_value_at_the_right_sample() {
    let harness = Harness::new();
    harness.start(48_000.0, 128);
    let mut block = Block::new(128, 1.0);
    let mut changes = FakeParameterChanges::new()
        // Half travel on the logarithmic cutoff.
        .with_queue(FakeParamQueue::new(2).with_point(0, 0.5))
        // Full travel on the linear gain, twice in one block.
        .with_queue(
            FakeParamQueue::new(1)
                .with_point(0, 0.0)
                .with_point(64, 1.0),
        );

    // SAFETY: every pointer is live for the call.
    unsafe {
        let audio = harness.audio();
        let mut data = block.data();
        data.input_parameter_changes = changes.as_com();
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut data),
            result::OK
        );
        release(audio);
    }

    // Three automation points became three DAUx events.
    assert_eq!(Counts::get(&harness.counts.events_seen), 3);

    // The controller's value follows the last point of the block.
    let harness_controller = harness.controller();
    // SAFETY: `harness_controller` is live.
    unsafe {
        let vtbl = controller_vtbl(harness_controller);
        assert!(((vtbl.get_param_normalized)(harness_controller, 1) - 1.0).abs() < 1e-12);
        assert!(((vtbl.get_param_normalized)(harness_controller, 2) - 0.5).abs() < 1e-12);
        release(harness_controller);
    }

    // …and the plug-in applied the *plain* values: the gain ended at +12 dB, which is what
    // the last block's samples were multiplied by.
    let expected = 10f32.powf(12.0 / 20.0);
    assert!(
        (block.output[0][127] - expected).abs() < 1e-3,
        "the plug-in saw a normalised value instead of a plain one: {}",
        block.output[0][127]
    );
}

#[test]
fn a_parameter_the_dsp_moves_reaches_the_hosts_automation_lane_normalised() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    harness
        .counts
        .emit_param_output
        .store(true, Ordering::Release);

    let mut block = Block::new(64, 0.0);
    let mut outgoing = FakeParameterChanges::new();
    // SAFETY: every pointer is live for the call.
    unsafe {
        let audio = harness.audio();
        let mut data = block.data();
        data.output_parameter_changes = outgoing.as_com();
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut data),
            result::OK
        );
        release(audio);
    }

    let lane = outgoing
        .queue(2)
        .expect("the plug-in's own parameter change must reach the host");
    assert_eq!(lane.points.len(), 1);
    let (offset, normalized) = lane.points[0];
    assert_eq!(offset, 3, "the sample offset must survive");
    assert!(
        (normalized - 1.0).abs() < 1e-9,
        "20 kHz is the top of a 20..20k logarithmic range, not {normalized}"
    );

    // …and the controller's mirror followed, so the editor sees it too.
    // SAFETY: `controller` is live.
    unsafe {
        let controller = harness.controller();
        assert!(
            ((controller_vtbl(controller).get_param_normalized)(controller, 2) - 1.0).abs() < 1e-9
        );
        release(controller);
    }
}

#[test]
fn a_host_with_no_output_lanes_is_tolerated_rather_than_assumed() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    harness
        .counts
        .emit_param_output
        .store(true, Ordering::Release);

    let mut block = Block::new(64, 0.0);
    // SAFETY: every pointer is live; `output_parameter_changes` is deliberately null.
    unsafe {
        let audio = harness.audio();
        let mut data = block.data();
        assert!(data.output_parameter_changes.is_null());
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut data),
            result::OK
        );
        release(audio);
    }
    assert_eq!(Counts::get(&harness.counts.processes), 1);
}

#[test]
fn a_value_the_host_sets_directly_reaches_the_dsp_on_the_next_block() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    // SAFETY: `controller` is live.
    unsafe {
        let controller = harness.controller();
        let vtbl = controller_vtbl(controller);
        // Full travel on the gain: +12 dB.
        assert_eq!((vtbl.set_param_normalized)(controller, 1, 1.0), result::OK);
        assert!(((vtbl.get_param_normalized)(controller, 1) - 1.0).abs() < 1e-12);
        // An unknown id and an out-of-range value are refused, not clamped into the table.
        assert_ne!(
            (vtbl.set_param_normalized)(controller, 999, 0.5),
            result::OK
        );
        release(controller);
    }

    let mut block = Block::new(64, 1.0);
    // SAFETY: every pointer is live.
    unsafe {
        let audio = harness.audio();
        let mut data = block.data();
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut data),
            result::OK
        );
        release(audio);
    }
    let expected = 10f32.powf(12.0 / 20.0);
    assert!(
        (block.output[0][0] - expected).abs() < 1e-3,
        "setParamNormalized never reached the DSP: {}",
        block.output[0][0]
    );
}

#[test]
fn parameter_text_round_trips_through_the_hosts_string_buffers() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    // SAFETY: `controller` is live and the buffers are live locals.
    unsafe {
        let controller = harness.controller();
        let vtbl = controller_vtbl(controller);

        let mut text = [0u16; 128];
        assert_eq!(
            (vtbl.get_param_string_by_value)(controller, 1, 1.0, text.as_mut_ptr()),
            result::OK
        );
        assert_eq!(utf16(&text), "12.00 dB");

        // The discrete one keeps the plug-in's own words.
        assert_eq!(
            (vtbl.get_param_string_by_value)(controller, 3, 0.0, text.as_mut_ptr()),
            result::OK
        );
        assert_eq!(utf16(&text), "1");

        // …and back the other way.
        let mut entered = [0u16; 128];
        crate::strings::write_utf16(&mut entered, "-6 dB");
        let mut normalized = -1.0;
        assert_eq!(
            (vtbl.get_param_value_by_string)(
                controller,
                1,
                entered.as_mut_ptr(),
                &raw mut normalized
            ),
            result::OK
        );
        assert!(((vtbl.normalized_param_to_plain)(controller, 1, normalized) + 6.0).abs() < 1e-9);

        // Nonsense is refused rather than parsed as zero.
        crate::strings::write_utf16(&mut entered, "banana");
        assert_ne!(
            (vtbl.get_param_value_by_string)(
                controller,
                1,
                entered.as_mut_ptr(),
                &raw mut normalized
            ),
            result::OK
        );
        // Null buffers are refused.
        assert_eq!(
            (vtbl.get_param_string_by_value)(controller, 1, 0.5, core::ptr::null_mut()),
            result::INVALID_ARGUMENT
        );
        release(controller);
    }
}

#[test]
fn the_editors_own_parameter_changes_reach_the_host_as_normalised_gestures() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    let mut handler = FakeComponentHandler::new();
    // SAFETY: `controller` and `handler` are live.
    unsafe {
        let controller = harness.controller();
        let vtbl = controller_vtbl(controller);
        assert_eq!(
            (vtbl.set_component_handler)(controller, handler.as_com()),
            result::OK
        );
        assert_eq!(
            handler.ref_count(),
            2,
            "the plug-in must retain the handler"
        );

        // Drive the host services the plug-in's controller was given, which is what an
        // editor does when the user grabs a knob.
        // SAFETY: the harness holds a reference to the component, so it is live.
        let component = &*harness.component.cast::<crate::component::Vst3Component>();
        let services = component.services();
        let params = services
            .params()
            .expect("the adapter always provides host params");
        params.gesture_begin(daux_plugin_api::ParamId(1));
        // A *plain* value in, a normalised one out: the conversion an editor must not have
        // to think about.
        params.changed(daux_plugin_api::ParamId(1), 12.0);
        params.gesture_end(daux_plugin_api::ParamId(1));

        assert_eq!(
            handler.calls,
            vec![
                HandlerCall::Begin(1),
                HandlerCall::Perform(1, 1.0),
                HandlerCall::End(1)
            ]
        );
        // The mirror followed too, so the host's next `getParamNormalized` agrees.
        assert!(((vtbl.get_param_normalized)(controller, 1) - 1.0).abs() < 1e-12);

        // Replacing the handler gives the old one back.
        assert_eq!(
            (vtbl.set_component_handler)(controller, core::ptr::null_mut()),
            result::OK
        );
        assert_eq!(handler.ref_count(), 1, "the handler must be released again");
        release(controller);
    }
}

// ---------------------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------------------

#[test]
fn notes_reach_the_plug_in_and_out_of_range_buses_are_dropped() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    let mut events = FakeEventList::new()
        .with(api::Event {
            bus_index: 0,
            sample_offset: 8,
            event_type: api::event_type::NOTE_ON,
            payload: api::EventPayload {
                note_on: api::NoteOnEvent {
                    channel: 0,
                    pitch: 60,
                    tuning: 0.0,
                    velocity: 1.0,
                    length: 0,
                    note_id: 1,
                },
            },
            ..api::Event::default()
        })
        .with(api::Event {
            // The spy has one event input port, so bus 4 does not exist.
            bus_index: 4,
            event_type: api::event_type::NOTE_OFF,
            ..api::Event::default()
        })
        .with(api::Event {
            // An event type VST3 has and DAUx does not.
            bus_index: 0,
            event_type: api::event_type::SCALE,
            ..api::Event::default()
        });

    let mut block = Block::new(64, 0.0);
    // SAFETY: every pointer is live for the call.
    unsafe {
        let audio = harness.audio();
        let mut data = block.data();
        data.input_events = events.as_com();
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut data),
            result::OK
        );
        release(audio);
    }
    assert_eq!(
        Counts::get(&harness.counts.events_seen),
        1,
        "only the note on the plug-in has a port for should have survived"
    );
}

// ---------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------

#[test]
fn state_round_trips_through_the_hosts_stream() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    let mut stream = VecStream::new();

    // SAFETY: every pointer is live.
    unsafe {
        let controller = harness.controller();
        let vtbl = controller_vtbl(controller);
        (vtbl.set_param_normalized)(controller, 1, 1.0);
        (vtbl.set_param_normalized)(controller, 2, 0.25);
        (vtbl.set_param_normalized)(controller, 3, 1.0);
        release(controller);

        // The DSP has to have the values before they can be saved.
        let mut block = Block::new(64, 0.0);
        let audio = harness.audio();
        let mut data = block.data();
        (audio_vtbl(audio).process)(audio, &raw mut data);
        release(audio);

        assert_eq!(
            (component_vtbl(harness.component).get_state)(harness.component, stream.as_com()),
            result::OK
        );
    }
    assert_eq!(Counts::get(&harness.counts.saves), 1);
    assert!(!stream.bytes().is_empty());
    assert!(
        stream.bytes().starts_with(b"DAUXST"),
        "the blob must be a daux-state document so presets are portable between formats"
    );

    // A fresh instance loads it back.
    let restored = Harness::new();
    restored.start(48_000.0, 64);
    let mut replay = VecStream::from_bytes(stream.bytes().to_vec());
    // SAFETY: every pointer is live.
    unsafe {
        assert_eq!(
            (component_vtbl(restored.component).set_state)(restored.component, replay.as_com()),
            result::OK
        );
        let controller = restored.controller();
        let vtbl = controller_vtbl(controller);
        assert!(((vtbl.get_param_normalized)(controller, 1) - 1.0).abs() < 1e-9);
        assert!(((vtbl.get_param_normalized)(controller, 2) - 0.25).abs() < 1e-9);
        assert!(((vtbl.get_param_normalized)(controller, 3) - 1.0).abs() < 1e-9);
        release(controller);
    }
    assert_eq!(Counts::get(&restored.counts.loads), 1);
}

#[test]
fn a_hostile_state_stream_is_refused_rather_than_trusted() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    // SAFETY: every pointer is live.
    unsafe {
        let component = harness.component;

        // Null.
        assert_eq!(
            (component_vtbl(component).set_state)(component, core::ptr::null_mut()),
            result::INVALID_ARGUMENT
        );
        // Empty.
        let mut empty = VecStream::new();
        assert_ne!(
            (component_vtbl(component).set_state)(component, empty.as_com()),
            result::OK
        );
        // Garbage.
        let mut garbage = VecStream::from_bytes(vec![0xFF; 512]);
        assert_ne!(
            (component_vtbl(component).set_state)(component, garbage.as_com()),
            result::OK
        );
        // Truncated: a real document with its tail cut off.
        let mut good = VecStream::new();
        (component_vtbl(component).get_state)(component, good.as_com());
        let half = good.bytes().len() / 2;
        let mut truncated = VecStream::from_bytes(good.bytes()[..half].to_vec());
        assert_ne!(
            (component_vtbl(component).set_state)(component, truncated.as_com()),
            result::OK
        );
        // A stream that fails every read.
        let mut failing = VecStream::failing();
        assert_ne!(
            (component_vtbl(component).set_state)(component, failing.as_com()),
            result::OK
        );

        // …and after all of that the instance still works.
        let mut block = Block::new(64, 0.25);
        let audio = harness.audio();
        let mut data = block.data();
        assert_eq!(
            (audio_vtbl(audio).process)(audio, &raw mut data),
            result::OK
        );
        release(audio);
    }
}

#[test]
fn the_controller_half_mirrors_the_components_state_without_touching_the_plug_in() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    let mut stream = VecStream::new();
    // SAFETY: every pointer is live.
    unsafe {
        let controller = harness.controller();
        let vtbl = controller_vtbl(controller);
        (vtbl.set_param_normalized)(controller, 1, 0.0);
        let mut block = Block::new(64, 0.0);
        let audio = harness.audio();
        let mut data = block.data();
        (audio_vtbl(audio).process)(audio, &raw mut data);
        release(audio);
        (component_vtbl(harness.component).get_state)(harness.component, stream.as_com());

        // Move the mirror somewhere else, then let `setComponentState` pull it back.
        (vtbl.set_param_normalized)(controller, 1, 1.0);
        assert!(((vtbl.get_param_normalized)(controller, 1) - 1.0).abs() < 1e-12);

        let mut replay = VecStream::from_bytes(stream.bytes().to_vec());
        assert_eq!(
            (vtbl.set_component_state)(controller, replay.as_com()),
            result::OK
        );
        assert!((vtbl.get_param_normalized)(controller, 1).abs() < 1e-9);
        assert_eq!(
            Counts::get(&harness.counts.loads),
            0,
            "setComponentState must not re-run the plug-in's own load_state"
        );

        // The controller has no state of its own, and says so without failing.
        let mut nothing = VecStream::new();
        assert_eq!((vtbl.get_state)(controller, nothing.as_com()), result::OK);
        assert_eq!((vtbl.set_state)(controller, nothing.as_com()), result::OK);
        release(controller);
    }
}

// ---------------------------------------------------------------------------------------
// Editors
// ---------------------------------------------------------------------------------------

#[test]
fn an_editor_can_be_opened_and_closed_repeatedly_while_audio_runs() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    let platform = crate::view::platform_type();
    // SAFETY: every pointer is live.
    unsafe {
        let controller = harness.controller();
        let vtbl = controller_vtbl(controller);

        for _ in 0..3 {
            let view = (vtbl.create_view)(controller, c"editor".as_ptr());
            assert!(!view.is_null(), "the spy has an editor");
            let v = view_vtbl(view);

            assert_eq!(
                (v.is_platform_type_supported)(view, platform.as_ptr().cast()),
                result::TRUE
            );
            assert_eq!(
                (v.is_platform_type_supported)(view, c"NotAPlatform".as_ptr()),
                result::FALSE
            );

            let mut rect = ViewRect::default();
            assert_eq!((v.get_size)(view, &raw mut rect), result::OK);
            assert_eq!(rect.width(), 640);
            assert_eq!(rect.height(), 480);
            assert_eq!((v.can_resize)(view), result::TRUE);

            let parent = core::ptr::without_provenance_mut::<c_void>(0x1234);
            assert_eq!(
                (v.attached)(view, parent, platform.as_ptr().cast()),
                result::OK
            );
            // Audio keeps running while the window is open — rule 9.
            let mut block = Block::new(64, 0.5);
            let audio = harness.audio();
            let mut data = block.data();
            assert_eq!(
                (audio_vtbl(audio).process)(audio, &raw mut data),
                result::OK
            );
            release(audio);

            let mut resize = ViewRect::sized(800, 600);
            assert_eq!((v.on_size)(view, &raw mut resize), result::OK);
            assert_eq!((v.removed)(view), result::OK);
            assert_eq!(release(view), 0);
        }

        // …and the DSP never noticed.
        assert_eq!(Counts::get(&harness.counts.processes), 3);
        assert_eq!(Counts::get(&harness.counts.deactivates), 0);
        release(controller);
    }
}

#[test]
fn a_headless_plug_in_hands_out_no_view() {
    let harness = Harness::headless();
    harness.start(48_000.0, 64);
    // SAFETY: `controller` is live.
    unsafe {
        let controller = harness.controller();
        let view = (controller_vtbl(controller).create_view)(controller, c"editor".as_ptr());
        assert!(
            view.is_null(),
            "a headless plug-in must not invent a window"
        );
        release(controller);
    }
}

#[test]
fn a_second_view_is_refused_and_an_unknown_view_name_is_too() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    // SAFETY: every pointer is live.
    unsafe {
        let controller = harness.controller();
        let vtbl = controller_vtbl(controller);
        let first = (vtbl.create_view)(controller, c"editor".as_ptr());
        assert!(!first.is_null());
        let second = (vtbl.create_view)(controller, c"editor".as_ptr());
        assert!(second.is_null(), "one editor, one window");
        assert!((vtbl.create_view)(controller, c"parameters".as_ptr()).is_null());

        assert_eq!(release(first), 0);
        // Once the first is gone, another can be made.
        let third = (vtbl.create_view)(controller, c"editor".as_ptr());
        assert!(!third.is_null());
        assert_eq!(release(third), 0);
        release(controller);
    }
}

#[test]
fn a_view_outlives_a_host_that_releases_the_plug_in_first() {
    let counts = Arc::new(Counts::default());
    let factory = Vst3Factory::create(Box::new(WatchedFactory {
        counts: Arc::clone(&counts),
        headless: false,
    }));
    let cid = crate::cid::class_id(SpyPlugin::ID);
    let mut component: *mut c_void = core::ptr::null_mut();
    // SAFETY: every pointer is live.
    unsafe {
        (factory_vtbl(factory).create_instance)(
            factory,
            core::ptr::from_ref(&cid).cast(),
            core::ptr::from_ref(&api::ICOMPONENT_IID).cast(),
            &raw mut component,
        );
        (component_vtbl(component).initialize)(component, core::ptr::null_mut());

        let controller = query(component, &api::IEDIT_CONTROLLER_IID);
        let view = (controller_vtbl(controller).create_view)(controller, c"editor".as_ptr());
        assert!(!view.is_null());
        release(controller);

        // The host drops the plug-in while its window is open — which hosts do.
        release(component);
        assert!(
            !counts.dropped.load(Ordering::Acquire),
            "the view still holds a reference; freeing here would be a use-after-free"
        );

        // The view still works, and only its release finishes the object off.
        let mut rect = ViewRect::default();
        assert_eq!((view_vtbl(view).get_size)(view, &raw mut rect), result::OK);
        assert_eq!(release(view), 0);
        assert!(counts.dropped.load(Ordering::Acquire));
        release(factory);
    }
}

// ---------------------------------------------------------------------------------------
// Panics
// ---------------------------------------------------------------------------------------

#[test]
fn a_panicking_process_returns_an_error_and_poisons_the_instance_for_ever() {
    let harness = Harness::new();
    harness.start(48_000.0, 64);
    harness
        .counts
        .panic_in_process
        .store(true, Ordering::Release);

    let mut block = Block::new(64, 0.5);
    // SAFETY: every pointer is live.
    let (first, second) = unsafe {
        let audio = harness.audio();
        let mut data = block.data();
        let first = quietly(|| (audio_vtbl(audio).process)(audio, &raw mut data));
        // The plug-in would panic again — but it must never be entered again at all.
        let second = (audio_vtbl(audio).process)(audio, &raw mut data);
        release(audio);
        (first, second)
    };
    assert_eq!(
        first,
        result::INTERNAL_ERROR,
        "a panic must become an error"
    );
    assert_eq!(
        second,
        result::NOT_INITIALIZED,
        "a poisoned instance must refuse rather than re-enter"
    );

    // Every other entry point is poisoned too, on every head.
    // SAFETY: every pointer is live.
    unsafe {
        let component = harness.component;
        assert_eq!(
            (component_vtbl(component).set_active)(component, 0),
            result::NOT_INITIALIZED
        );
        let mut stream = VecStream::new();
        assert_eq!(
            (component_vtbl(component).get_state)(component, stream.as_com()),
            result::NOT_INITIALIZED
        );
        let controller = harness.controller();
        assert_eq!(
            (controller_vtbl(controller).get_parameter_count)(controller),
            0
        );
        assert_eq!(
            (controller_vtbl(controller).set_param_normalized)(controller, 1, 0.5),
            result::NOT_INITIALIZED
        );
        assert!(
            (controller_vtbl(controller).create_view)(controller, c"editor".as_ptr()).is_null()
        );
        let audio = harness.audio();
        assert_eq!((audio_vtbl(audio).get_latency_samples)(audio), 0);
        release(audio);
        release(controller);
    }

    // …and the object is still destroyable, which is the whole point of poisoning rather
    // than aborting.
    drop(harness);
}

#[test]
fn a_panicking_prepare_poisons_at_set_active_rather_than_unwinding_into_the_host() {
    let harness = Harness::new();
    harness
        .counts
        .panic_in_prepare
        .store(true, Ordering::Release);

    // SAFETY: every pointer is live.
    unsafe {
        let component = harness.component;
        assert_eq!(
            (component_vtbl(component).initialize)(component, core::ptr::null_mut()),
            result::OK
        );
        let status = quietly(|| (component_vtbl(component).set_active)(component, 1));
        assert_eq!(status, result::INTERNAL_ERROR);
        assert_eq!(
            (component_vtbl(component).set_active)(component, 1),
            result::NOT_INITIALIZED
        );
        assert_eq!(
            (component_vtbl(component).terminate)(component),
            result::NOT_INITIALIZED
        );
    }
}

#[test]
fn a_poisoned_instance_can_still_be_released_without_leaking_the_plug_in() {
    let counts = Arc::new(Counts::default());
    let factory = Vst3Factory::create(Box::new(WatchedFactory {
        counts: Arc::clone(&counts),
        headless: false,
    }));
    let cid = crate::cid::class_id(SpyPlugin::ID);
    let mut component: *mut c_void = core::ptr::null_mut();
    // SAFETY: every pointer is live.
    unsafe {
        (factory_vtbl(factory).create_instance)(
            factory,
            core::ptr::from_ref(&cid).cast(),
            core::ptr::from_ref(&api::ICOMPONENT_IID).cast(),
            &raw mut component,
        );
        (component_vtbl(component).initialize)(component, core::ptr::null_mut());
        counts.panic_in_prepare.store(true, Ordering::Release);
        quietly(|| (component_vtbl(component).set_active)(component, 1));

        assert_eq!(release(component), 0);
        assert!(
            counts.dropped.load(Ordering::Acquire),
            "poisoning must not leak the plug-in"
        );
        release(factory);
    }
}

// ---------------------------------------------------------------------------------------
// Connection points
// ---------------------------------------------------------------------------------------

#[test]
fn the_connection_point_accepts_a_peer_and_refuses_nonsense() {
    let harness = Harness::new();
    // SAFETY: every pointer is live.
    unsafe {
        let connection = query(harness.component, &api::ICONNECTION_POINT_IID);
        assert!(!connection.is_null());
        let vtbl = &**connection.cast::<*const IConnectionPointVtbl>();
        // Hosts refuse to load a plug-in whose `connect` fails.
        assert_eq!((vtbl.connect)(connection, harness.component), result::OK);
        assert_eq!(
            (vtbl.connect)(connection, core::ptr::null_mut()),
            result::INVALID_ARGUMENT
        );
        assert_eq!((vtbl.disconnect)(connection, harness.component), result::OK);
        assert_eq!(
            (vtbl.notify)(connection, core::ptr::null_mut()),
            result::INVALID_ARGUMENT
        );
        release(connection);
    }
}

// ---------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------

/// Reads a null-terminated ASCII field out of a `#[repr(C)]` struct.
fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Reads a null-terminated UTF-16 field out of a `#[repr(C)]` struct.
fn utf16(units: &[u16]) -> String {
    // SAFETY: `units` is a live slice of exactly the length it reports.
    unsafe { crate::strings::read_utf16(units.as_ptr(), units.len()) }
}

// The macro a plug-in crate writes, expanded here so that a change to it cannot compile
// only in documentation. The exported symbols land in the test binary, which is harmless and
// is also the only way to prove `#[unsafe(no_mangle)] extern "system"` still applies to them.
crate::export_entry!(SingleFactory<SpyPlugin>);

#[test]
fn the_exported_entry_point_the_macro_emits_produces_a_working_factory() {
    let factory = GetPluginFactory();
    assert!(!factory.is_null());
    // SAFETY: `factory` is live and carries one reference this test owns.
    unsafe {
        let vtbl = factory_vtbl(factory);
        assert_eq!((vtbl.count_classes)(factory), 1);
        let mut info = core::mem::zeroed::<PClassInfo>();
        assert_eq!((vtbl.get_class_info)(factory, 0, &raw mut info), result::OK);
        assert_eq!(cstr(&info.name), "Spy");
        assert_eq!(release(factory), 0);
    }

    #[cfg(target_os = "windows")]
    {
        assert!(InitDll());
        assert!(ExitDll());
    }
}

#[test]
fn the_compatibility_report_is_reachable_from_the_crate_root() {
    let descriptor = SpyPlugin::descriptor();
    let report = crate::compatibility_report(&descriptor);
    assert!(
        report.is_empty(),
        "a plain stereo effect with a GUI translates losslessly: {report:?}"
    );
}
