# Real-time safety

The audio callback runs on a thread with a hard deadline. At 48 kHz with a 64-sample
buffer, every `process` call must finish in **1.33 ms**, every time, including the worst
case. There is no scheduler that will forgive a miss: a late buffer is an audible click,
and in a session with 200 plug-in instances, one of them misbehaving ruins the whole take.

This document defines what "real-time safe" means in DAUxPlug, how the SDK enforces it, and
what to do instead of the things you can't do.

---

## 1. The rule

**Inside `process` — and inside everything `process` calls — the following are forbidden:**

| Forbidden                          | Why it breaks                                                     |
| ---------------------------------- | ----------------------------------------------------------------- |
| Heap allocation / deallocation     | The allocator takes a lock and may call into the OS (unbounded)    |
| `Mutex`, `RwLock`, any blocking lock | Priority inversion: a non-RT thread can hold it for milliseconds |
| File, network, or IPC syscalls     | Unbounded, and page faults block on disk                           |
| Formatting (`format!`, `println!`) | Allocates, and typically locks stdout                              |
| `dlopen` / `LoadLibrary`           | Takes the loader lock, does I/O                                    |
| Thread creation, `join`, `sleep`   | Unbounded by definition                                            |
| Any GUI or windowing call          | Locks, allocates, and may block on the compositor                  |
| Waiting on a worker's result       | Turns a bounded callback into an unbounded one                     |
| Unbounded loops (`while queue.pop()`) | Input size must bound the work                                   |
| `panic!`, `unwrap`, `expect`, indexing that can fail | Unwinding across FFI is UB; aborting kills the DAW |
| Lazy initialisation on first use   | The first block after activation is exactly when you can't afford it |

Everything on that list is an *unbounded* or *unpredictable* operation. The test isn't
"is it usually fast", it's "does it have a worst case I can state in nanoseconds".

## 2. Where work actually goes

```
                 allocate, load, compute tables, build voices
                                    │
   ┌────────────────────────────────┴─────────────────────────────────┐
   │                                                                   │
prepare(config)                 activate()                    editor open
[main-thread]                  [main-thread]                  [main-thread]
   │                                                                   │
   └──────────────► preallocated, bounded, ready ◄─────────────────────┘
                                    │
                                 process()
                              [audio-thread]
                     reads params, runs DSP, writes output
                                    │
              ┌─────────────────────┴──────────────────────┐
              ▼                                            ▼
     request_callback()                          push to bounded queue
     → on_main_thread()                          → UI reads a snapshot
```

`prepare` is told `max_block_size`; that is your allocation budget. `process` may be
called with **any** frame count from 1 to that maximum, so size for the maximum and index
by the actual.

## 3. What to do instead

**Instead of allocating a scratch buffer** — allocate it in `prepare` and reuse it:

```rust
fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
    self.scratch = ScratchBuffers::new(2, config.max_block_size as usize); // [main-thread]
    Ok(())
}
```

**Instead of a `Mutex<Vec<f32>>` shared with the UI** — use a `TripleBuffer` (the writer
never waits) or an SPSC ring buffer:

```rust
// audio thread
self.spectrum_tx.write(|dst| dst.copy_from_slice(&self.fft_out));  // never blocks
// UI thread
let latest = self.spectrum_rx.read();
```

**Instead of loading a file or building a table on demand** — schedule it:

```rust
if self.needs_new_impulse {
    ctx.host().schedule_worker(TaskId(IMPULSE_RELOAD));  // returns false if the queue is full
}
// ...later, off the audio thread:
fn on_worker(&mut self, task: TaskId) { /* load, then hand over via a queue */ }
```

**Instead of `log::info!("gain = {gain}")`** — enqueue a bounded record and let the
consumer format it:

```rust
ctx.host().log(LogLevel::Warn, "voice steal");   // fixed-size record, no formatting
```

**Instead of `unwrap()`** — return `ProcessStatus::Error` and let the host silence the
output. A plug-in that returns an error is a plug-in the user can still remove; a plug-in
that panics may take the session with it.

**Instead of growing an output event list** — accept that `try_push` can fail:

```rust
if events.output().try_push(&note_off).is_err() {
    // The host's queue is full. Drop it, or retry next block. Do NOT allocate.
}
```

## 4. Parameters on the audio thread

Parameters are read, never locked. Each parameter's value lives in an atomic, so the
processor, the controller and the editor share one `Arc<Params>` with no synchronisation
beyond the atomic itself:

```rust
let target = self.params.gain.plain() as f32;   // [any-thread], lock-free
self.gain_smoother.set_target(target);          // [audio-thread]
for frame in 0..frames {
    out[frame] = inp[frame] * self.gain_smoother.next();
}
```

Sample-accurate automation arrives as `ParamValue` events in the input event list, already
sorted by time. Process the block in segments between event timestamps rather than
re-reading the atomic per sample.

## 5. Enforcement

**Debug assertions.** `daux_rt::rt_assert_audio_thread!()` compiles away in release and
fires in debug when a `[audio-thread]` API is called from the wrong thread.

**Allocation detection in tests.** `daux-rt` ships a counting allocator that the test
harness installs. A test can assert that a whole block of processing allocated exactly
zero times:

```rust
let (status, allocs) = AllocGuard::scope(|| processor.process(&ctx, &mut audio, &mut events));
assert_eq!(allocs, 0, "process() allocated {allocs} times");
assert_eq!(status, ProcessStatus::Continue);
```

`tests/harness/tests/realtime.rs` runs this against every example plug-in, across a sweep
of block sizes including 1, prime sizes, and `max_block_size`, with and without events.

**Type-level hints.** Audio-thread services are handed out as `RtHostServices`, which
simply does not have the blocking methods on it. The compiler stops most mistakes before
the allocator counter has to.

**Review.** Every `[audio-thread]` doc annotation is a claim that the item obeys this
document. Changing an item from `[main-thread]` to `[audio-thread]` requires proving it.

## 6. Denormals, NaNs and other silent killers

A denormal float can be 100× slower to process on some CPUs — a real-time failure that
never shows up in a functional test. Feedback paths (filters, delays, reverbs) must either
add a tiny DC offset, flush small values to zero, or set FTZ/DAZ where the host permits it.
`daux-dsp` does this in every recursive structure it ships and documents which method it
uses.

NaN propagates: one NaN in a delay line poisons the output forever, and the user hears
silence with no way to recover but reloading the plug-in. Validate at the boundaries
(parameter input, incoming audio if you feed it back) rather than per sample.

## 7. Multi-instance reality

Calls to one instance are never concurrent with each other, but calls to *different*
instances are — and a given instance may migrate between audio threads between blocks.
Therefore:

- No `static mut`, no global caches, no lazily-initialised shared tables without a
  `OnceLock` whose initialisation happens off the audio thread.
- No thread-local state that assumes it will see the same thread next block.
- Shared read-only tables (wavetables, window functions) are fine — build them once at
  factory creation, share them behind an `Arc`, and never mutate them.

## 8. Offline mode is not an excuse

In `ProcessMode::Offline` the deadline disappears, but determinism does not. A plug-in may
take a slower, higher-quality path offline, but the same input must produce the same
output on every run, on every machine, at the same sample rate. Do not branch on wall-clock
time, thread count, or CPU feature detection in a way that changes the result.
