# Sandboxing

A host that loads third-party plug-ins in-process is one `null` dereference away from
losing the user's session. Sandboxing moves that risk into a process the host can kill and
restart. DAUxPlug does not ship complete sandbox transports in v1 — but the architecture
was built so that adding them is an implementation job, not a redesign.

## Modes

```
InProcess   the plug-in library is loaded into the host process        (v1, complete)
Sandboxed   the plug-in runs in a child process; audio moves through
            shared memory, control through a framed message channel    (v1: protocol + loopback)
Remote      the plug-in runs on another machine                        (future)
```

```
        ┌──────────────┐   control frames    ┌────────────────────┐   in-proc   ┌──────────┐
        │     host     │◄──────────────────►│  DAUx runtime      │◄───────────►│ plug-in  │
        │              │                     │  process           │   DAUx ABI  │ binary   │
        │              │   shared memory     │                    │             │          │
        │              │◄──────────────────►│                    │             │          │
        └──────────────┘   audio + events    └────────────────────┘             └──────────┘
```

The child process is a *DAUx runtime*, not the plug-in itself. It speaks the same DAUx C
ABI to the plug-in that an in-process host would, so **the plug-in cannot tell the
difference** and needs no sandbox-specific code. That is the whole point of having the ABI
be the boundary rather than a Rust trait.

## Why the in-process design already works out-of-process

Three decisions made in v1 are what make this possible:

1. **No memory crosses the module boundary.** Every buffer is caller-owned and callee-filled,
   or borrowed for one call. Nothing to marshal, nothing to keep alive across a process
   boundary, no allocator contract to reconcile.
2. **The audio-thread API is already bounded queues and flat records.** `DauxEventListV1` is
   a function table over opaque storage; the storage can be a slice of shared memory just as
   easily as a `Vec` in the host. `DauxProcessV1` is pointers plus counts.
3. **Every host service is optional and negotiated.** A sandboxed runtime that cannot offer
   a service simply doesn't advertise it, and plug-ins already have to handle that.

Had any of these gone the other way — Rust types across the boundary, a growable event list,
mandatory services — the sandbox would require a second, incompatible plug-in API.

## Control plane vs data plane

They have opposite requirements, so they are separate types with separate transports.

| | Control plane | Data plane |
| --- | --- | --- |
| Carries | create/destroy, activate, state, editor, latency, errors | audio blocks, events, transport |
| Latency budget | milliseconds | sub-millisecond, every block |
| Failure mode | retry, report, restart | drop the block, output silence |
| Encoding | length-prefixed binary frames | fixed `#[repr(C)]` records in shared memory |
| Allocation | allowed | forbidden |

`daux-protocol` defines both. Neither uses `serde` or JSON: the data plane cannot afford it,
and the control plane is parsed from a possibly-hostile peer, where a hand-written,
bounds-checked, length-capped decoder is easier to reason about than a derive.

JSON is fine for diagnostics that leave the runtime entirely — logs, crash reports, scan
caches. Never on either plane.

## Decoding is a security boundary

The peer may be a crashed process writing garbage, or a malicious one writing carefully
chosen garbage. Every decoder in `daux-protocol` therefore:

- validates a magic + version header before anything else;
- caps frame size before allocating, and never allocates based on an unvalidated length;
- bounds-checks every field read against the remaining buffer;
- returns `Err` on malformed input and never panics;
- is tested against truncated, corrupted and adversarially sized frames.

The same rules apply to `daux-bundle` parsing manifests, for the same reason: it is untrusted
input from the internet.

## Crash handling

When the runtime process dies:

1. The host notices — the control channel closes, or the liveness deadline passes.
2. The audio thread keeps running. It sees no fresh data in the shared region and outputs
   silence for that instance. **It does not block waiting for a dead peer**, which is why
   the data plane is polled, never awaited.
3. The host marks the instance failed, keeps its last known state, and offers to restart it.
4. On restart, the runtime reloads the plug-in and replays the saved state. The user loses
   audio for a moment, not their session.

`daux-ipc` models liveness and timeout policy as traits, so this logic is testable against
the loopback transport without killing real processes.

## What ships in v1

| Piece | State |
| --- | --- |
| Control-plane message set and framing codec | Implemented, tested |
| Data-plane `#[repr(C)]` record layouts | Implemented, layout-asserted |
| `LoopbackTransport` (in-process, backed by `daux-rt` queues) | Implemented, tested |
| `ControlChannel` framing over any transport | Implemented |
| Liveness / timeout policy traits | Implemented |
| Windows named pipes, Unix domain sockets, shared memory mapping | Declared, not implemented |
| Editor transport (`ExternalWindow`) | Modelled, not implemented |
| GPU resource sharing across processes | Modelled in the shared-texture extension |

The loopback path is not a toy: it is the same code path the real transports will use, with
the OS handle replaced by a queue. When the platform transports land, the message layer,
the codec, the crash policy and the tests do not change.

## What sandboxing is not

It is not a substitute for the plug-in behaving well. A sandboxed plug-in that allocates on
the audio thread still glitches — it just glitches in a process the host can kill. And it is
not free: an extra copy through shared memory and a process hop cost latency and CPU. In-process
remains the default for trusted plug-ins, which is why the mode is a host policy decision
rather than a property of the plug-in.
