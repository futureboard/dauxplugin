//! The whole path, driven against the in-process module in [`crate::fake`].
//!
//! These are the tests that would catch a regression a host actually notices: a lifecycle
//! call made out of order, an instance destroyed while still processing, a module unloaded
//! while an instance is alive, a panic that is not converted into a poisoned instance.

use std::sync::Arc;

use daux_abi::{
    DAUX_ERR_INVALID_STATE, DAUX_ERR_PANIC, DAUX_ERR_UNSUPPORTED, DAUX_PROCESS_CONTINUE,
    DAUX_PROCESS_SLEEP, DAUX_WINDOW_API_COCOA, DAUX_WINDOW_API_WIN32, DauxEventNoteV1,
    DauxEventParamV1,
};
use daux_core::{Category, ProcessConfig, ProcessMode, ProcessStatus, Tail};
use daux_host_services::HostServices;
use daux_parameter::ParamId;
use daux_transport::TransportBuilder;

use crate::fake::{self, Behaviour, Call, GAIN_ID, Journal, STATE_BLOB, SYNTH_ID, TableShape};
use crate::{
    AxtModule, HostBlock, HostBridge, LoadedFactory, LoadedPlugin, PluginState, RuntimeErrorKind,
};

/// Arms the fake module and builds a factory from it.
fn boot(behaviour: Behaviour) -> (Arc<Journal>, Arc<AxtModule>, LoadedFactory) {
    let journal = fake::install(behaviour);
    let module = Arc::new(fake::module());
    let factory = LoadedFactory::create(Arc::clone(&module), HostBridge::new(HostServices::null()))
        .expect("the fake module publishes a conforming factory");
    (journal, module, factory)
}

fn gain_instance(
    behaviour: Behaviour,
) -> (Arc<Journal>, Arc<AxtModule>, LoadedFactory, LoadedPlugin) {
    let (journal, module, factory) = boot(behaviour);
    let plugin = factory
        .create_plugin(GAIN_ID)
        .expect("the fake factory publishes this id");
    (journal, module, factory, plugin)
}

// --------------------------------------------------------------- the happy path ----

#[test]
fn a_module_walks_from_bundle_shaped_load_to_a_processed_block() {
    let (journal, _module, factory, mut plugin) = gain_instance(Behaviour::default());

    assert_eq!(factory.plugin_count(), 2);
    let descriptors = factory.descriptors().expect("both descriptors convert");
    assert_eq!(descriptors[0].id, GAIN_ID);
    assert_eq!(descriptors[0].category, Category::Effect);
    assert_eq!(descriptors[1].id, SYNTH_ID);
    assert_eq!(descriptors[1].category, Category::Instrument);
    assert_eq!(descriptors[0].features, ["test", "fake"]);
    assert_eq!(descriptors[0].state_schema_version, 2);

    let config = ProcessConfig::new(44_100.0, 128).with_process_mode(ProcessMode::Offline);
    plugin.activate(&config).expect("activate");
    assert_eq!(plugin.lifecycle(), PluginState::Active);
    plugin.start_processing().expect("start");
    assert_eq!(plugin.lifecycle(), PluginState::Processing);

    let mut input = [0.25f32; 16];
    let mut left = [0.0f32; 16];
    let mut right = [0.0f32; 16];
    let mut block = HostBlock::new(&[1], &[2], 128);
    block.set_frames(16).unwrap();
    block.bind_input(0, 0, &mut input).unwrap();
    block.bind_output(0, 0, &mut left).unwrap();
    block.bind_output(0, 1, &mut right).unwrap();
    block.set_transport(Some(&TransportBuilder::new().tempo(96.0).build()));

    let mut note = DauxEventNoteV1::new();
    note.header.time = 4;
    note.key = 60;
    block.input_events_mut().push_note(&note).unwrap();

    assert_eq!(plugin.process(&mut block), ProcessStatus::Continue);

    // The module saw everything the host bound.
    assert!(journal.contains(&Call::Process {
        frames: 16,
        input_channels: 1,
        first_input: 0.25,
        events_in: 1,
        tempo: 96.0,
    }));
    // And its output event came back.
    assert_eq!(block.output_events().len(), 1);
    assert_eq!(block.output_events().note(0).unwrap().key, 71);

    // The block holds the sample buffers borrowed for as long as it exists, which is the
    // point of the `'a` on `HostBlock`; reading them back means letting it go first.
    drop(block);
    assert_eq!(left, [0.5f32; 16], "the module wrote through the bindings");
    assert_eq!(right, [0.5f32; 16]);

    plugin.stop_processing().expect("stop");
    plugin.deactivate().expect("deactivate");
    drop(plugin);

    // The lifecycle reached the module in the order `abi-v1` §7 fixes.
    let order = [
        Call::CreateFactory,
        Call::CreatePlugin(GAIN_ID.to_owned()),
        Call::Init,
        Call::Activate {
            sample_rate: 44_100.0,
            max_block: 128,
            mode: daux_abi::DAUX_PROCESS_MODE_OFFLINE,
            format: daux_abi::DAUX_SAMPLE_FORMAT_F32,
        },
        Call::StartProcessing,
        Call::StopProcessing,
        Call::Deactivate,
        Call::Destroy,
    ];
    let positions: Vec<usize> = order
        .iter()
        .map(|call| {
            journal
                .index_of(call)
                .unwrap_or_else(|| panic!("{call:?} never reached the module"))
        })
        .collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "the lifecycle calls arrived out of order: {positions:?}"
    );
    assert_eq!(journal.calls().last(), Some(&Call::Destroy));
    // The factory is still alive, so it has not been destroyed yet.
    assert!(!journal.contains(&Call::DestroyFactory));
    drop(factory);
    assert!(journal.contains(&Call::DestroyFactory));
}

/// The invariant the whole crate exists for: the module cannot be unloaded while anything
/// derived from it is alive, whatever order the host drops things in.
#[test]
fn the_module_outlives_every_object_derived_from_it() {
    let (journal, module, factory, plugin) = gain_instance(Behaviour::default());
    assert_eq!(
        Arc::strong_count(&module),
        2,
        "the test's handle plus the factory's"
    );

    // Dropping the host's own factory handle changes nothing while an instance lives.
    drop(factory);
    assert_eq!(Arc::strong_count(&module), 2);
    assert!(
        !journal.contains(&Call::DestroyFactory),
        "an instance still exists, so `abi-v1` §5 forbids destroying the factory"
    );

    drop(plugin);
    assert!(journal.contains(&Call::Destroy));
    assert!(
        journal.contains(&Call::DestroyFactory),
        "the factory goes with the last instance"
    );
    assert_eq!(
        Arc::strong_count(&module),
        1,
        "and only then does the module become droppable"
    );
}

/// A second instance keeps the factory alive too, and each gets its own state.
#[test]
fn instances_are_independent_and_each_holds_the_factory() {
    let (journal, module, factory, first) = gain_instance(Behaviour::default());
    let second = factory.create_plugin(SYNTH_ID).expect("a second instance");
    assert_eq!(Arc::strong_count(&module), 2);

    drop(factory);
    drop(first);
    assert!(
        !journal.contains(&Call::DestroyFactory),
        "one instance is still alive"
    );
    drop(second);
    assert_eq!(journal.count(&Call::Destroy), 2);
    assert!(journal.contains(&Call::DestroyFactory));
    assert_eq!(Arc::strong_count(&module), 1);
}

// ------------------------------------------------------- refusing broken modules ----

#[test]
fn a_failing_create_factory_is_reported_verbatim() {
    let journal = fake::install(Behaviour {
        create_factory_status: DAUX_ERR_UNSUPPORTED.0,
        ..Behaviour::default()
    });
    let module = Arc::new(fake::module());
    let err = LoadedFactory::create(module, HostBridge::new(HostServices::null()))
        .expect_err("the module refused");
    assert_eq!(err.kind(), RuntimeErrorKind::Unsupported);
    assert_eq!(err.status(), Some(DAUX_ERR_UNSUPPORTED.0));
    assert!(journal.contains(&Call::CreateFactory));
    assert!(
        !journal.contains(&Call::DestroyFactory),
        "nothing was created, so nothing is destroyed"
    );
}

#[test]
fn a_factory_that_reports_success_and_publishes_nothing_is_refused() {
    let journal = fake::install(Behaviour {
        factory_table: TableShape::Null,
        ..Behaviour::default()
    });
    let module = Arc::new(fake::module());
    let err = LoadedFactory::create(module, HostBridge::new(HostServices::null()))
        .expect_err("no table, no factory");
    assert_eq!(err.kind(), RuntimeErrorKind::Protocol);
    assert!(journal.contains(&Call::CreateFactory));
}

/// An undersized table must be refused *and* the factory behind it destroyed, because the
/// module allocated something even though the host cannot use it.
#[test]
fn an_undersized_factory_table_is_refused_and_the_factory_still_destroyed() {
    let journal = fake::install(Behaviour {
        factory_table: TableShape::Undersized,
        ..Behaviour::default()
    });
    let module = Arc::new(fake::module());
    let err = LoadedFactory::create(module, HostBridge::new(HostServices::null()))
        .expect_err("a table this host may not call into");
    assert_eq!(err.kind(), RuntimeErrorKind::AbiMismatch);
    assert!(
        journal.contains(&Call::DestroyFactory),
        "the factory object must not be leaked just because its table is unusable"
    );
}

#[test]
fn an_unknown_plugin_id_is_not_found_rather_than_a_protocol_failure() {
    let (journal, _module, factory) = boot(Behaviour::default());
    let err = factory
        .create_plugin("com.example.does-not-exist")
        .expect_err("no such plug-in");
    assert_eq!(err.kind(), RuntimeErrorKind::NotFound);
    assert!(journal.contains(&Call::CreatePlugin("com.example.does-not-exist".to_owned())));
    assert!(!journal.contains(&Call::Init));
}

#[test]
fn an_instance_whose_init_fails_is_destroyed_not_leaked() {
    let (journal, _module, factory) = boot(Behaviour {
        init_status: DAUX_ERR_INVALID_STATE.0,
        ..Behaviour::default()
    });
    let err = factory.create_plugin(GAIN_ID).expect_err("init refused");
    assert_eq!(err.kind(), RuntimeErrorKind::InvalidState);
    assert!(journal.contains(&Call::Init));
    assert!(
        journal.contains(&Call::Destroy),
        "`abi-v1` §7: an instance that failed `init` still has to be destroyed"
    );
}

#[test]
fn an_instance_that_publishes_no_table_is_refused() {
    let (_journal, _module, factory) = boot(Behaviour {
        plugin_table: TableShape::Null,
        ..Behaviour::default()
    });
    let err = factory.create_plugin(GAIN_ID).expect_err("no table");
    assert_eq!(err.kind(), RuntimeErrorKind::Protocol);
}

#[test]
fn an_undersized_instance_table_is_refused_before_init_runs() {
    let (journal, _module, factory) = boot(Behaviour {
        plugin_table: TableShape::Undersized,
        ..Behaviour::default()
    });
    let err = factory.create_plugin(GAIN_ID).expect_err("unusable table");
    assert_eq!(err.kind(), RuntimeErrorKind::AbiMismatch);
    assert!(
        !journal.contains(&Call::Init),
        "nothing may be called through a table this host has refused"
    );
}

#[test]
fn a_descriptor_the_module_never_writes_is_refused() {
    let (_journal, _module, factory) = boot(Behaviour {
        descriptor_writes_nothing: true,
        ..Behaviour::default()
    });
    let err = factory.descriptor(0).expect_err("an empty descriptor");
    assert_eq!(err.kind(), RuntimeErrorKind::Protocol);
    assert!(factory.descriptors().is_err());
}

#[test]
fn a_descriptor_with_a_malformed_id_is_refused() {
    let (_journal, _module, factory) = boot(Behaviour {
        descriptor_id: Some("not a reverse dns id"),
        ..Behaviour::default()
    });
    let err = factory.descriptor(0).expect_err("a malformed id");
    assert_eq!(err.kind(), RuntimeErrorKind::Protocol);
}

#[test]
fn a_descriptor_index_past_the_end_is_not_found() {
    let (_journal, _module, factory) = boot(Behaviour::default());
    let err = factory.descriptor(2).expect_err("only two plug-ins");
    assert_eq!(err.kind(), RuntimeErrorKind::NotFound);
    let err = factory
        .descriptor(usize::MAX)
        .expect_err("does not fit the ABI's u32");
    assert_eq!(err.kind(), RuntimeErrorKind::InvalidArgument);
}

// ------------------------------------------------------------------- lifecycle ----

/// Every transition `abi-v1` §7 forbids, refused on this side of the boundary so the
/// module never has to defend itself against its host.
#[test]
fn transitions_the_specification_forbids_never_reach_the_module() {
    let (journal, _module, _factory, mut plugin) = gain_instance(Behaviour::default());

    // `process` before `start_processing`.
    let mut samples = [0.0f32; 8];
    let mut block = HostBlock::new(&[], &[1], 64);
    block.set_frames(8).unwrap();
    block.bind_output(0, 0, &mut samples).unwrap();
    assert_eq!(plugin.process(&mut block), ProcessStatus::Error);

    // `start_processing` before `activate`.
    assert_eq!(
        plugin.start_processing().unwrap_err().kind(),
        RuntimeErrorKind::InvalidState
    );
    // `deactivate` before `activate`.
    assert_eq!(
        plugin.deactivate().unwrap_err().kind(),
        RuntimeErrorKind::InvalidState
    );
    // `stop_processing` before `start_processing`.
    assert_eq!(
        plugin.stop_processing().unwrap_err().kind(),
        RuntimeErrorKind::InvalidState
    );

    let config = ProcessConfig::new(48_000.0, 64);
    plugin.activate(&config).unwrap();
    // `activate` twice.
    assert_eq!(
        plugin.activate(&config).unwrap_err().kind(),
        RuntimeErrorKind::InvalidState
    );
    // `reset` is legal while active.
    plugin.reset().unwrap();

    plugin.start_processing().unwrap();
    // `deactivate` while processing.
    assert_eq!(
        plugin.deactivate().unwrap_err().kind(),
        RuntimeErrorKind::InvalidState
    );
    // `reset` while processing is the one state §7 forbids it in.
    assert_eq!(
        plugin.reset().unwrap_err().kind(),
        RuntimeErrorKind::InvalidState
    );

    let calls = journal.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, Call::Process { .. }))
            .count(),
        0,
        "no `process` may have reached the module"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| **c
                == Call::Activate {
                    sample_rate: 48_000.0,
                    max_block: 64,
                    mode: 0,
                    format: 1
                })
            .count(),
        1,
        "the refused second `activate` must not have been forwarded"
    );
    assert_eq!(journal.count(&Call::Reset), 1, "only the legal reset ran");
    assert_eq!(journal.count(&Call::Deactivate), 0);
}

/// A host that drops an instance mid-run is making a mistake; taking the process down with
/// it would turn that mistake into a crash in an unrelated plug-in.
#[test]
fn dropping_a_processing_instance_walks_it_back_to_inactive_first() {
    let (journal, _module, _factory, mut plugin) = gain_instance(Behaviour::default());
    plugin.activate(&ProcessConfig::new(48_000.0, 64)).unwrap();
    plugin.start_processing().unwrap();
    assert_eq!(plugin.lifecycle(), PluginState::Processing);
    drop(plugin);

    let calls = journal.calls();
    let stop = calls.iter().position(|c| *c == Call::StopProcessing);
    let deactivate = calls.iter().position(|c| *c == Call::Deactivate);
    let destroy = calls.iter().position(|c| *c == Call::Destroy);
    assert!(stop < deactivate, "stop_processing must precede deactivate");
    assert!(deactivate < destroy, "deactivate must precede destroy");
}

#[test]
fn an_invalid_process_config_never_reaches_the_module() {
    let (journal, _module, _factory, mut plugin) = gain_instance(Behaviour::default());
    let mut bad = ProcessConfig::new(0.0, 64);
    assert_eq!(
        plugin.activate(&bad).unwrap_err().kind(),
        RuntimeErrorKind::InvalidArgument
    );
    bad = ProcessConfig::new(48_000.0, 0);
    assert_eq!(
        plugin.activate(&bad).unwrap_err().kind(),
        RuntimeErrorKind::InvalidArgument
    );
    assert!(
        !journal
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Activate { .. })),
        "a configuration the model rejects must not be forwarded"
    );
    assert_eq!(plugin.lifecycle(), PluginState::Inactive);
}

/// `abi-v1` §17: a caught panic poisons the instance, and a poisoned instance is
/// unloadable-but-safe — it refuses work rather than taking the host down.
#[test]
fn a_panic_status_poisons_the_instance_and_everything_afterwards_is_refused() {
    let (_journal, _module, _factory, mut plugin) = gain_instance(Behaviour {
        activate_status: DAUX_ERR_PANIC.0,
        ..Behaviour::default()
    });

    let err = plugin
        .activate(&ProcessConfig::new(48_000.0, 64))
        .expect_err("the module reported a caught panic");
    assert_eq!(err.kind(), RuntimeErrorKind::Poisoned);
    assert!(plugin.is_poisoned());
    assert_eq!(plugin.lifecycle(), PluginState::Inactive);

    assert_eq!(
        plugin
            .activate(&ProcessConfig::new(48_000.0, 64))
            .unwrap_err()
            .kind(),
        RuntimeErrorKind::Poisoned
    );
    assert_eq!(
        plugin.start_processing().unwrap_err().kind(),
        RuntimeErrorKind::Poisoned
    );
    assert_eq!(
        plugin.reset().unwrap_err().kind(),
        RuntimeErrorKind::Poisoned
    );
    assert_eq!(
        plugin.on_main_thread().unwrap_err().kind(),
        RuntimeErrorKind::Poisoned
    );
    assert!(
        plugin.params().is_none(),
        "a poisoned instance publishes nothing"
    );
    assert!(plugin.state().is_none());
    assert!(plugin.gui().is_none());
    assert_eq!(plugin.latency(), 0);
    assert_eq!(plugin.tail(), Tail::None);
    // And dropping it is still safe.
    drop(plugin);
}

#[test]
fn a_failing_start_processing_leaves_the_instance_active() {
    let (_journal, _module, _factory, mut plugin) = gain_instance(Behaviour {
        start_status: DAUX_ERR_INVALID_STATE.0,
        ..Behaviour::default()
    });
    plugin.activate(&ProcessConfig::new(48_000.0, 64)).unwrap();
    assert!(plugin.start_processing().is_err());
    assert_eq!(
        plugin.lifecycle(),
        PluginState::Active,
        "a refused start must not advance the state machine"
    );
    plugin.deactivate().expect("still deactivatable");
}

// ---------------------------------------------------------------------- process ----

#[test]
fn a_block_longer_than_the_activation_never_reaches_the_module() {
    let (journal, _module, _factory, mut plugin) = gain_instance(Behaviour::default());
    plugin.activate(&ProcessConfig::new(48_000.0, 32)).unwrap();
    plugin.start_processing().unwrap();

    // The block itself allows 64 frames; the activation does not.
    let mut samples = [0.0f32; 64];
    let mut block = HostBlock::new(&[], &[1], 64);
    block.set_frames(64).unwrap();
    block.bind_output(0, 0, &mut samples).unwrap();
    assert_eq!(plugin.process(&mut block), ProcessStatus::Error);
    assert!(
        !journal
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Process { .. }))
    );

    block.set_frames(32).unwrap();
    assert_eq!(plugin.process(&mut block), ProcessStatus::Continue);
}

#[test]
fn an_unbound_channel_never_reaches_the_module() {
    let (journal, _module, _factory, mut plugin) = gain_instance(Behaviour::default());
    plugin.activate(&ProcessConfig::new(48_000.0, 64)).unwrap();
    plugin.start_processing().unwrap();

    let mut block = HostBlock::new(&[], &[2], 64);
    block.set_frames(8).unwrap();
    let mut only_left = [0.0f32; 8];
    block.bind_output(0, 0, &mut only_left).unwrap();
    assert_eq!(
        plugin.process(&mut block),
        ProcessStatus::Error,
        "the right channel is still null"
    );
    assert!(
        !journal
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Process { .. }))
    );
}

/// The output list must start empty every block, and come back sorted whatever order the
/// plug-in pushed in (`abi-v1` §9).
#[test]
fn output_events_are_cleared_per_block_and_sorted_afterwards() {
    let (_journal, _module, _factory, mut plugin) = gain_instance(Behaviour::default());
    plugin.activate(&ProcessConfig::new(48_000.0, 64)).unwrap();
    plugin.start_processing().unwrap();

    let mut samples = [0.0f32; 8];
    let mut block = HostBlock::new(&[], &[1], 64);
    block.set_frames(8).unwrap();
    block.bind_output(0, 0, &mut samples).unwrap();

    // A stale event from a previous block, which must not survive.
    let mut stale = DauxEventNoteV1::new();
    stale.header.time = 99;
    stale.key = 1;
    block.output_events_mut().push_note(&stale).unwrap();

    assert_eq!(plugin.process(&mut block), ProcessStatus::Continue);
    assert_eq!(block.output_events().len(), 1, "the stale event is gone");
    assert_eq!(block.output_events().note(0).unwrap().key, 71);
    assert_eq!(block.output_events().note(0).unwrap().header.time, 7);

    // A second block starts clean again.
    assert_eq!(plugin.process(&mut block), ProcessStatus::Continue);
    assert_eq!(block.output_events().len(), 1);
}

#[test]
fn a_process_result_is_reported_as_the_model_status() {
    let (_journal, _module, _factory, mut plugin) = gain_instance(Behaviour {
        process_result: DAUX_PROCESS_SLEEP,
        ..Behaviour::default()
    });
    plugin.activate(&ProcessConfig::new(48_000.0, 64)).unwrap();
    plugin.start_processing().unwrap();
    let mut samples = [0.0f32; 8];
    let mut block = HostBlock::new(&[], &[1], 64);
    block.set_frames(8).unwrap();
    block.bind_output(0, 0, &mut samples).unwrap();
    assert_eq!(plugin.process(&mut block), ProcessStatus::Sleep);
}

/// A result code from a newer ABI must degrade to the conservative reading — "keep
/// calling" — because falsely putting a plug-in to sleep cuts off audio, while calling one
/// that had nothing to say only costs a block.
#[test]
fn an_unknown_process_result_degrades_conservatively() {
    let (_journal, _module, _factory, mut plugin) = gain_instance(Behaviour {
        process_result: 999,
        ..Behaviour::default()
    });
    plugin.activate(&ProcessConfig::new(48_000.0, 64)).unwrap();
    plugin.start_processing().unwrap();
    let mut samples = [0.0f32; 8];
    let mut block = HostBlock::new(&[], &[1], 64);
    block.set_frames(8).unwrap();
    block.bind_output(0, 0, &mut samples).unwrap();
    let status = plugin.process(&mut block);
    assert_ne!(999, DAUX_PROCESS_CONTINUE, "999 is not a v1 result code");
    assert_eq!(status, ProcessStatus::Continue);
    assert!(status.must_keep_calling());
}

/// The audio thread may not allocate. The block is preallocated, so a whole run of blocks
/// has to be allocation-free — including the event traffic in both directions.
#[test]
fn a_run_of_blocks_allocates_nothing() {
    use daux_core::daux_rt::{AllocGuard, counting_allocator_installed};

    assert!(
        counting_allocator_installed(),
        "the allocation tripwire is not installed, so this test would pass vacuously"
    );
    let (_journal, _module, _factory, mut plugin) = gain_instance(Behaviour::default());
    plugin.activate(&ProcessConfig::new(48_000.0, 64)).unwrap();
    plugin.start_processing().unwrap();

    let mut input = [0.1f32; 64];
    let mut output = [0.0f32; 64];
    let mut block = HostBlock::new(&[1], &[1], 64);
    block.set_frames(64).unwrap();
    block.bind_input(0, 0, &mut input).unwrap();
    block.bind_output(0, 0, &mut output).unwrap();

    // Warm every lazily-taken path once, outside the measured scope.
    let mut param = DauxEventParamV1::new();
    param.param_id = 1;
    param.value = -3.0;
    block.input_events_mut().push_param(&param).unwrap();
    assert_eq!(plugin.process(&mut block), ProcessStatus::Continue);

    let ((), allocations) = AllocGuard::scope(|| {
        for _ in 0..64 {
            block.input_events_mut().clear();
            let _ = block.input_events_mut().push_param(&param);
            let _ = plugin.process(&mut block);
        }
    });
    assert_eq!(
        allocations, 0,
        "the audio-thread path allocated {allocations} times"
    );
}

// ------------------------------------------------------------------- extensions ----

#[test]
fn the_parameter_model_round_trips() {
    let (journal, _module, _factory, plugin) = gain_instance(Behaviour::default());
    let params = plugin.params().expect("the fake publishes daux.params/1");
    assert_eq!(params.count(), 2);

    let all = params.all().expect("both parameters");
    assert_eq!(all[0].id, ParamId(1));
    assert_eq!(all[0].name, "Gain");
    assert_eq!(all[0].unit, "dB");
    assert_eq!(all[0].group, "Main");
    assert_eq!(all[0].min, -60.0);
    assert_eq!(all[0].max, 12.0);
    assert!(
        all[0]
            .flags
            .contains(daux_parameter::ParamFlags::AUTOMATABLE)
    );
    assert_eq!(all[1].id, ParamId(7));

    assert_eq!(params.value(ParamId(1)).unwrap(), 0.5);
    assert_eq!(params.value_to_text(ParamId(1), -6.0).unwrap(), "-6.00");
    assert_eq!(params.text_to_value(ParamId(1), " -3.5 ").unwrap(), -3.5);

    // Unknown ids are `NotFound`, not a crash and not a silent zero.
    assert_eq!(
        params.value(ParamId(999)).unwrap_err().kind(),
        RuntimeErrorKind::NotFound
    );
    assert_eq!(
        params.info(99).unwrap_err().kind(),
        RuntimeErrorKind::NotFound
    );
    assert_eq!(
        params
            .text_to_value(ParamId(1), "not a number")
            .unwrap_err()
            .kind(),
        RuntimeErrorKind::Status
    );

    // `flush` carries events in both directions while the instance is idle.
    let mut input = crate::EventList::with_capacity(4, 512);
    let mut output = crate::EventList::with_capacity(4, 512);
    let mut param = DauxEventParamV1::new();
    param.param_id = 1;
    param.value = -12.0;
    input.push_param(&param).unwrap();
    params.flush(&mut input, &mut output);
    assert!(journal.contains(&Call::ParamsFlush(1)));
    assert_eq!(output.len(), 1);
    assert_eq!(output.note(0).unwrap().key, 12);
}

#[test]
fn state_round_trips_through_a_host_owned_stream() {
    let (journal, _module, _factory, plugin) = gain_instance(Behaviour::default());
    let state = plugin.state().expect("the fake publishes daux.state/1");

    let blob = state.save().expect("save");
    assert_eq!(
        blob, STATE_BLOB,
        "chunked writes must be reassembled in order"
    );
    assert!(journal.contains(&Call::StateSave));

    state.load(&blob).expect("load");
    assert!(
        journal.contains(&Call::StateLoad(STATE_BLOB.len())),
        "the module must see exactly the bytes the host handed over"
    );

    // An empty blob is legal and reads as an immediate end of stream.
    state.load(&[]).expect("an empty blob loads");
    assert!(journal.contains(&Call::StateLoad(0)));
}

#[test]
fn a_failing_save_or_load_is_reported_rather_than_producing_a_half_blob() {
    let (_journal, _module, _factory, plugin) = gain_instance(Behaviour {
        save_status: daux_abi::DAUX_ERR_IO.0,
        ..Behaviour::default()
    });
    let err = plugin
        .state()
        .unwrap()
        .save()
        .expect_err("the module refused");
    assert_eq!(err.kind(), RuntimeErrorKind::Status);
    assert_eq!(err.status(), Some(daux_abi::DAUX_ERR_IO.0));

    let (_journal, _module, _factory, plugin) = gain_instance(Behaviour {
        load_status: daux_abi::DAUX_ERR_VERSION.0,
        ..Behaviour::default()
    });
    let err = plugin
        .state()
        .unwrap()
        .load(b"from the future")
        .expect_err("refused");
    assert_eq!(
        err.status(),
        Some(daux_abi::DAUX_ERR_VERSION.0),
        "abi-v1 §12: an unreadable schema version is `DAUX_ERR_VERSION`, with no side effects"
    );
}

#[test]
fn the_editor_can_be_opened_and_closed_without_touching_the_dsp() {
    let (journal, _module, _factory, mut plugin) = gain_instance(Behaviour::default());
    plugin.activate(&ProcessConfig::new(48_000.0, 64)).unwrap();
    plugin.start_processing().unwrap();

    {
        let gui = plugin.gui().expect("the fake publishes daux.gui/1");
        assert!(gui.is_api_supported(DAUX_WINDOW_API_WIN32, false));
        assert!(!gui.is_api_supported(DAUX_WINDOW_API_COCOA, false));
        assert_eq!(
            gui.create(DAUX_WINDOW_API_COCOA, false).unwrap_err().kind(),
            RuntimeErrorKind::Unsupported
        );
        gui.create(DAUX_WINDOW_API_WIN32, false).expect("create");
        assert_eq!(gui.size().unwrap(), (640, 480));
        assert!(gui.can_resize());
        gui.set_size(800, 600).expect("resize");
        assert_eq!(
            gui.set_size(0, 600).unwrap_err().kind(),
            RuntimeErrorKind::Status
        );
        // Optional entries this plug-in leaves null.
        assert_eq!(
            gui.set_scale(2.0).unwrap_err().kind(),
            RuntimeErrorKind::Unsupported
        );
        assert_eq!(
            gui.adjust_size(801, 601).unwrap(),
            (801, 601),
            "a null `adjust_size` means any size is accepted"
        );
        gui.show().unwrap();
        gui.hide().unwrap();
        gui.destroy();
    }

    assert!(journal.contains(&Call::GuiCreate));
    assert!(journal.contains(&Call::GuiDestroy));
    assert_eq!(
        plugin.lifecycle(),
        PluginState::Processing,
        "the editor's lifetime is independent of the processor's"
    );
    assert_eq!(journal.count(&Call::Reset), 0);
    assert_eq!(journal.count(&Call::Deactivate), 0);
}

#[test]
fn latency_and_tail_come_from_their_extensions() {
    let (_journal, _module, _factory, plugin) = gain_instance(Behaviour {
        latency: 512,
        tail: 4_096,
        ..Behaviour::default()
    });
    assert_eq!(plugin.latency(), 512);
    assert_eq!(plugin.tail(), Tail::Samples(4_096));

    let (_journal, _module, _factory, infinite) = gain_instance(Behaviour {
        tail: daux_abi::DAUX_TAIL_INFINITE,
        ..Behaviour::default()
    });
    assert_eq!(infinite.tail(), Tail::Infinite);
}

/// An absent extension is a normal answer, not an error: the host must degrade.
#[test]
fn absent_extensions_degrade_rather_than_fail() {
    let (_journal, _module, _factory, plugin) = gain_instance(Behaviour {
        with_params: false,
        with_state: false,
        with_gui: false,
        with_latency: false,
        with_tail: false,
        ..Behaviour::default()
    });
    assert!(plugin.params().is_none());
    assert!(plugin.state().is_none());
    assert!(plugin.gui().is_none());
    assert_eq!(plugin.latency(), 0);
    assert_eq!(plugin.tail(), Tail::None);
    assert!(plugin.extension("com.example.made-up/1").is_null());
}

/// An extension table the host may not call into has to be indistinguishable from an
/// absent one — anything else means calling through it.
#[test]
fn an_undersized_extension_table_is_treated_as_absent() {
    let (_journal, _module, _factory, plugin) = gain_instance(Behaviour {
        params_table: TableShape::Undersized,
        ..Behaviour::default()
    });
    assert!(
        plugin.params().is_none(),
        "a table below the v1.0 minimum must not be called into"
    );
    assert!(
        !plugin.extension(daux_abi::ext::PARAMS).is_null(),
        "the module does publish something; it is the runtime that refuses it"
    );
    // The extensions that are fine still work.
    assert!(plugin.state().is_some());
}

#[test]
fn on_main_thread_reaches_the_module() {
    let (journal, _module, _factory, mut plugin) = gain_instance(Behaviour::default());
    plugin.on_main_thread().unwrap();
    assert!(journal.contains(&Call::OnMainThread));
}

#[test]
fn the_factory_reports_the_module_it_came_from() {
    let (_journal, module, factory, plugin) = gain_instance(Behaviour::default());
    assert!(Arc::ptr_eq(factory.module(), &module));
    assert!(Arc::ptr_eq(plugin.module(), &module));
    assert_eq!(factory.module().sdk_name(), "daux-runtime-fake");
    assert_eq!(factory.module().abi_version(), (1, 0));
    assert!(factory.extension("com.example.anything/1").is_null());
    assert!(factory.host().services().params().is_none());
}
