# Threading model

DAUxPlug never asks "which thread am I on?" at runtime if it can help it. Instead, every
API states its thread class up front, and the type system hands out different capabilities
to different threads.

## Thread classes

| Class      | Owner  | Characteristics                                                       |
| ---------- | ------ | --------------------------------------------------------------------- |
| **Main**   | host   | Plug-in lifecycle, parameters metadata, state, GUI. Blocking tolerated. |
| **Audio**  | host   | `process` and friends. Hard deadline, [realtime rules](realtime.md).   |
| **UI**     | host   | On most platforms identical to Main; on some, a distinct thread.        |
| **Worker** | host   | Runs `on_worker` for work the plug-in scheduled from the audio thread.  |
| **Scanner**| host   | Enumerates bundles. May be a separate process.                          |
| **IPC**    | runtime| Moves control frames for sandboxed plug-ins.                            |
| **Unknown**| —      | Anything else. `[any-thread]` APIs must survive it.                     |

The annotations `[main-thread]`, `[audio-thread]` and `[any-thread]` on every public item
are normative — they mirror `docs/specifications/abi-v1.md` §15.

## Guarantees the host gives

1. `process` for one instance is never concurrent with another `process` for that instance.
2. `process` for one instance **may** be concurrent with `process` for a different
   instance, on a different thread.
3. The audio thread may change between blocks. No thread-local state survives.
4. `activate`/`deactivate`/`init`/`destroy` and every GUI call happen on the main thread,
   never concurrently with `process` for that instance.
5. `daux.params/1::flush` is called on the main thread while inactive, on the audio thread
   while active — the one method with a dual class, and it is documented as such.

## What crosses threads, and how

```
        main thread                     audio thread                    UI thread
             │                               │                              │
   set_plain(id, value) ──── atomic ────────►│                              │
             │                               │                              │
             │◄──── request_callback() ──────┤ (bounded, lock-free)         │
             │      on_main_thread()         │                              │
             │                               │                              │
             │                               ├──── TripleBuffer ───────────►│  meters,
             │                               │     (never blocks)           │  spectra
             │                               │                              │
             │◄──────────── gesture_begin / changed / gesture_end ──────────┤  automation
             │                               │                              │  from widgets
             │                               │                              │
     schedule(task) ◄──── bounded queue ─────┤                              │
     [worker thread runs on_worker]          │                              │
```

Three mechanisms, and only three:

- **Atomics** for scalar parameter values. `Param::plain()` and `set_plain()` are lock-free
  and callable from any thread.
- **Bounded lock-free queues** (`SpscRingBuffer`, `MpscQueue`) for anything with a payload:
  events out, worker requests, log records.
- **`TripleBuffer`** for "the UI wants the latest snapshot and doesn't care about the ones
  it missed": meter values, FFT frames, waveform overviews.

There is no fourth mechanism. If you find yourself wanting a `Mutex` shared with the audio
thread, you want one of these instead.

## Runtime thread identity

`ThreadCheck::is_audio_thread()` exists because the ABI exposes it, and it is genuinely
useful for debug assertions and for adapters that must behave differently in a host that
calls a method from an unexpected place. It is **not** an invitation to branch:

```rust
// Wrong: two behaviours, twice the bugs, and the fast path is now unpredictable.
if ctx.thread().is_audio_thread() { fast() } else { slow_and_allocating() }

// Right: separate entry points, each with one contract.
fn process(..)          { /* [audio-thread] */ }
fn on_main_thread(..)   { /* [main-thread]  */ }
```

Use it in `debug_assert!` and in the SDK's own defensive checks. Ship no logic that depends
on it.

## Editor lifetime

An editor is created and destroyed on the main thread, possibly many times, while the
processor keeps running on the audio thread untouched. The editor and the processor share
exactly one thing: the `Arc<Params>` and whatever bounded queues the plug-in set up between
them. Closing an editor must drop only UI resources — never DSP state, never a queue the
audio thread still writes to.

Because the queues outlive the editor, they are owned by the plug-in instance, not by the
editor. The editor borrows the reader end.
