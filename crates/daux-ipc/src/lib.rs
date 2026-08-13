//! Transport-agnostic control-plane and data-plane IPC for sandboxed DAUx plug-ins.
//!
//! `daux-protocol` defines what the bytes mean. This crate moves them, and hands out the
//! memory the audio lives in. Nothing here decides *policy* — which plug-in to load, when
//! to restart it — and nothing here parses a message: it is the plumbing between the wire
//! format and whoever is using it.
//!
//! # Two planes, two shapes
//!
//! | | Control plane | Data plane |
//! |---|---|---|
//! | Type | [`ControlTransport`] → [`ControlChannel`] | [`DataPlane`] over [`SharedRegion`] |
//! | Carries | lifecycle, state, editor, errors | one block of audio and events |
//! | Shape | a byte stream, reassembled into frames | a mapped buffer, handed back and forth |
//! | Budget | milliseconds, may allocate | sub-millisecond, allocates nothing |
//! | When it fails | retry, report, restart | drop the block, output silence |
//!
//! They are separate because their constraints are opposite, and mixing them would make the
//! audio path pay for the control path's flexibility. See
//! `docs/architecture/sandboxing.md`.
//!
//! # What is real in v1
//!
//! [`LoopbackTransport`] and [`LoopbackDataPlane`] are complete, in-process
//! implementations — two lock-free queues and a heap allocation, no operating system in
//! sight. They are not stand-ins: they are the same code path the sandboxed configuration
//! will run, with the OS handle replaced. The framing, the reassembly, the header
//! validation, the ownership handshake and the crash policy are all exercised by them.
//!
//! The platform transports in [`platform`] are declared and refuse cleanly with
//! [`IpcErrorKind::Unsupported`]. They never panic and never silently succeed, so a host
//! that asks this build to sandbox is told no and can fall back to loading in process.
//!
//! # A control conversation, end to end
//!
//! ```
//! use daux_ipc::{ControlChannel, LoopbackTransport};
//! use daux_protocol::{ControlMessage, InstanceId, RequestId};
//!
//! let (host_end, sandbox_end) = LoopbackTransport::pair();
//! let mut host = ControlChannel::new(host_end);
//! let mut sandbox = ControlChannel::new(sandbox_end);
//!
//! host.send(&ControlMessage::CreateInstance {
//!     request: RequestId(1),
//!     instance: InstanceId(1),
//!     plugin_id: "studio.futureboard.gain".to_owned(),
//!     bundle_path: "C:/plugins/Gain.axt".to_owned(),
//! })?;
//!
//! let Some(ControlMessage::CreateInstance { request, instance, .. }) = sandbox.poll()? else {
//!     unreachable!("the frame was sent before the poll")
//! };
//! sandbox.send(&ControlMessage::Ack { request, instance })?;
//!
//! assert!(matches!(host.poll()?, Some(ControlMessage::Ack { .. })));
//! # Ok::<(), daux_ipc::IpcError>(())
//! ```
//!
//! # A block of audio, end to end
//!
//! ```
//! use daux_ipc::{DataPlane, LoopbackDataPlane};
//! use daux_protocol::{AudioBlockLayout, ProtocolLimits};
//!
//! let limits = ProtocolLimits::new();
//! let layout = AudioBlockLayout::new(2, 2, 512);
//! let (mut host, mut sandbox) = LoopbackDataPlane::pair(&layout, 1, &limits)?;
//!
//! // The host owns the region to begin with, and hands it over for block 1.
//! assert_eq!(host.acquire(0)?, 0);
//! host.publish(0, 1)?;
//!
//! // Only now can the sandbox touch it.
//! assert_eq!(sandbox.acquire(0)?, 1);
//! let region = &mut sandbox.audio_regions_mut()[0];
//! // SAFETY: `acquire` succeeded, so this endpoint owns the region and nothing else is
//! // reading or writing it until it is published back.
//! let header = unsafe { region.read_header(&limits) }?;
//! assert_eq!(header.frame_count, 512);
//! sandbox.publish(0, 1)?;
//! # Ok::<(), daux_ipc::IpcError>(())
//! ```
//!
//! # Thread annotations
//!
//! Every public item carries `[audio-thread]`, `[main-thread]` or `[any-thread]`, matching
//! `docs/specifications/abi-v1.md` §15. Only [`DataPlane`] and [`SharedRegion`] are
//! `[audio-thread]`; the control plane never is.

#![deny(unsafe_op_in_unsafe_fn)]

mod channel;
mod dataplane;
mod error;
mod liveness;
mod loopback;
pub mod platform;
mod region;
mod transport;

pub use channel::ControlChannel;
pub use dataplane::{DataPlane, DataPlaneEndpoint, LoopbackDataPlane};
pub use error::{IpcError, IpcErrorKind, IpcResult};
pub use liveness::{LivenessPolicy, PeerHealth};
pub use loopback::LoopbackTransport;
pub use region::{RegionRole, SharedRegion};
pub use transport::{ControlTransport, is_would_block};

/// The crate's own tests run under the counting allocator so that "the data plane does not
/// allocate" is checked rather than asserted. Nothing outside `cfg(test)` picks this up, so
/// production builds keep the platform allocator untouched.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_rt::CountingAllocator = daux_rt::CountingAllocator;

#[cfg(test)]
mod tests {
    use crate::{ControlChannel, DataPlane, LoopbackDataPlane, LoopbackTransport, SharedRegion};
    use daux_protocol::{
        AudioBlockLayout, ControlMessage, InstanceId, ProtocolLimits, RequestId, Tail,
    };

    const fn assert_send<T: Send>() {}

    /// The contract requires that anything crossing to the audio thread is `Send`. An
    /// accidental `Rc` in a field would otherwise only surface in a downstream crate.
    #[test]
    fn the_public_types_have_the_thread_bounds_the_contract_promises() {
        assert_send::<LoopbackTransport>();
        assert_send::<ControlChannel<LoopbackTransport>>();
        assert_send::<SharedRegion>();
        assert_send::<LoopbackDataPlane>();
        assert_send::<crate::IpcError>();
    }

    #[test]
    fn the_counting_allocator_is_installed_so_the_alloc_assertions_are_not_vacuous() {
        assert!(
            daux_rt::counting_allocator_installed(),
            "daux-ipc's own tests must run under CountingAllocator"
        );
    }

    /// The whole point of the crate, in one test: a host and a sandbox exchanging control
    /// messages *and* audio, on the paths that ship.
    #[test]
    fn a_host_and_a_sandbox_run_a_whole_instance_over_the_loopback() {
        let limits = ProtocolLimits::new();
        let (host_end, sandbox_end) = LoopbackTransport::pair();
        // A small receive cap, so the control channel really does reassemble.
        let mut host = ControlChannel::new(host_end.with_max_recv_chunk(5));
        let mut sandbox = ControlChannel::new(sandbox_end.with_max_recv_chunk(7));

        let layout = AudioBlockLayout::new(2, 2, 128);
        let (mut host_plane, mut sandbox_plane) =
            LoopbackDataPlane::pair(&layout, 1, &limits).unwrap();

        // ---- create ------------------------------------------------------------------
        host.send(&ControlMessage::CreateInstance {
            request: RequestId(1),
            instance: InstanceId(1),
            plugin_id: "studio.futureboard.gain".to_owned(),
            bundle_path: "C:/plugins/Gain.axt".to_owned(),
        })
        .unwrap();
        let request = match sandbox.poll().unwrap() {
            Some(ControlMessage::CreateInstance { request, .. }) => request,
            other => panic!("expected CreateInstance, got {other:?}"),
        };
        sandbox
            .send(&ControlMessage::Ack {
                request,
                instance: InstanceId(1),
            })
            .unwrap();
        assert!(matches!(
            host.poll().unwrap(),
            Some(ControlMessage::Ack { .. })
        ));

        // ---- eight blocks of audio ---------------------------------------------------
        let header = {
            let region = &host_plane.audio_regions()[0];
            // SAFETY: the host owns every region until it publishes one.
            unsafe { region.read_header(&limits) }.unwrap()
        };
        for block in 1..=8u64 {
            host_plane.acquire(0).unwrap();
            {
                let region = &mut host_plane.audio_regions_mut()[0];
                // SAFETY: the host acquired the region and has not published it, so it has
                // exclusive access for the whole block.
                let bytes = unsafe { region.bytes_mut(header.input_offset, 128 * 4) }.unwrap();
                for chunk in bytes.chunks_exact_mut(4) {
                    chunk.copy_from_slice(&(block as f32).to_le_bytes());
                }
            }
            host_plane.publish(0, block).unwrap();

            assert_eq!(sandbox_plane.acquire(0).unwrap(), block);
            {
                let region = &mut sandbox_plane.audio_regions_mut()[0];
                // SAFETY: the sandbox acquired the region, so ownership was transferred to
                // it; the input borrow is copied out before the output borrow is taken.
                let input: Vec<f32> = unsafe { region.input_plane_f32(&header, 0) }
                    .unwrap()
                    .to_vec();
                assert!(input.iter().all(|s| *s == block as f32));
                // SAFETY: as above.
                let output = unsafe { region.output_plane_f32_mut(&header, 0) }.unwrap();
                for (out, inp) in output.iter_mut().zip(&input) {
                    *out = inp * 0.5;
                }
            }
            sandbox_plane.publish(0, block).unwrap();

            assert_eq!(host_plane.acquire(0).unwrap(), block);
            let region = &mut host_plane.audio_regions_mut()[0];
            // SAFETY: the host acquired the region back, so it owns it again.
            let output = unsafe { region.output_plane_f32_mut(&header, 0) }.unwrap();
            assert_eq!(output.len(), 128);
            assert!(
                output.iter().all(|s| *s == block as f32 * 0.5),
                "the sandbox's work came back through the shared region"
            );
        }

        // ---- the sandbox reports something back, then the host tears down ------------
        sandbox
            .send(&ControlMessage::ReportTail {
                instance: InstanceId(1),
                tail: Tail::Samples(1024),
            })
            .unwrap();
        assert_eq!(
            host.poll().unwrap(),
            Some(ControlMessage::ReportTail {
                instance: InstanceId(1),
                tail: Tail::Samples(1024),
            })
        );
        host.close();
        assert!(!sandbox.is_open());
    }
}
