//! Reading and writing the host's `DauxStreamV1` (abi-v1 §11.3).
//!
//! The host owns the stream and therefore the allocation, so no memory crosses the boundary in
//! either direction. What does cross is a length the host chose, which is why every read is
//! bounded: a hostile or broken stream must not be able to make a plug-in allocate until the
//! process dies.

use daux_abi::{
    DAUX_ERR_INVALID_ARG, DAUX_ERR_IO, DAUX_ERR_OUT_OF_MEMORY, DauxStatus, DauxStreamV1,
};

/// Bytes pulled from the host in one call. A page-ish buffer: large enough that a real preset
/// is one or two calls, small enough to sit on the stack.
const CHUNK: usize = 8 * 1024;

/// [main-thread] Drains the stream, refusing to grow past `limit` bytes.
///
/// abi-v1 §11.3 says a short read means end of stream. This reader is deliberately more
/// forgiving and keeps going until the stream reports **zero** bytes: for a conforming host the
/// two are identical apart from one extra call, and for a host backed by a pipe, a socket or a
/// compressed file — all of which return short reads for reasons that have nothing to do with
/// EOF — the difference is a preset that loads instead of one that silently loses its tail.
///
/// # Errors
///
/// * [`DAUX_ERR_INVALID_ARG`] when the stream has no `read` entry;
/// * [`DAUX_ERR_IO`] when a read reports a negative count, or claims to have transferred more
///   than it was asked for;
/// * [`DAUX_ERR_OUT_OF_MEMORY`] when the stream is longer than `limit`.
///
/// # Safety
///
/// `stream` is null or points at a [`DauxStreamV1`] valid for the duration of the call.
pub(crate) unsafe fn read_all(
    stream: *const DauxStreamV1,
    limit: usize,
) -> Result<Vec<u8>, DauxStatus> {
    if stream.is_null() {
        return Err(DAUX_ERR_INVALID_ARG);
    }
    // SAFETY: non-null was checked; the caller guarantees the structure is live for the call.
    let stream = unsafe { &*stream };
    if !stream.is_v1_0_compatible() {
        return Err(daux_abi::DAUX_ERR_ABI_MISMATCH);
    }
    let Some(read) = stream.read else {
        return Err(DAUX_ERR_INVALID_ARG);
    };

    let mut out = Vec::new();
    let mut chunk = [0u8; CHUNK];
    loop {
        // SAFETY: `read` is an entry of a table the host owns and validated above; `chunk` is a
        // live, writable buffer of exactly `CHUNK` bytes for the duration of the call.
        let transferred = unsafe { read(stream.ctx, chunk.as_mut_ptr(), CHUNK) };
        if transferred < 0 {
            return Err(DAUX_ERR_IO);
        }
        let transferred = transferred as usize;
        if transferred > CHUNK {
            // The host claims to have written past the end of the buffer it was given. It has
            // not — but nothing it says can be trusted after that, including the bytes.
            return Err(DAUX_ERR_IO);
        }
        if transferred == 0 {
            // End of stream. This is also the guard that stops a stream which keeps making no
            // progress from spinning here forever.
            return Ok(out);
        }
        if out.len().saturating_add(transferred) > limit {
            return Err(DAUX_ERR_OUT_OF_MEMORY);
        }
        out.extend_from_slice(&chunk[..transferred]);
    }
}

/// [main-thread] Writes every byte of `bytes` to the stream.
///
/// # Errors
///
/// [`DAUX_ERR_INVALID_ARG`] when the stream has no `write` entry, and [`DAUX_ERR_IO`] when a
/// write fails or makes no progress — a stream that keeps accepting zero bytes would otherwise
/// spin here forever.
///
/// # Safety
///
/// As [`read_all`].
pub(crate) unsafe fn write_all(
    stream: *const DauxStreamV1,
    bytes: &[u8],
) -> Result<(), DauxStatus> {
    if stream.is_null() {
        return Err(DAUX_ERR_INVALID_ARG);
    }
    // SAFETY: non-null was checked; the caller guarantees the structure is live for the call.
    let stream = unsafe { &*stream };
    if !stream.is_v1_0_compatible() {
        return Err(daux_abi::DAUX_ERR_ABI_MISMATCH);
    }
    let Some(write) = stream.write else {
        return Err(DAUX_ERR_INVALID_ARG);
    };

    let mut written = 0usize;
    while written < bytes.len() {
        let remaining = &bytes[written..];
        // SAFETY: `write` is an entry of a table the host owns and validated above; the slice
        // is live and readable for the duration of the call.
        let transferred = unsafe { write(stream.ctx, remaining.as_ptr(), remaining.len()) };
        if transferred <= 0 {
            // Negative is a failure; zero is no progress, which would loop forever.
            return Err(DAUX_ERR_IO);
        }
        let transferred = transferred as usize;
        if transferred > remaining.len() {
            return Err(DAUX_ERR_IO);
        }
        written += transferred;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use core::ffi::c_void;
    use std::cell::RefCell;

    /// A host-side stream over a `Vec<u8>`, with the failure modes a plug-in has to survive.
    pub(crate) struct FakeStream {
        inner: Box<Inner>,
        table: DauxStreamV1,
    }

    pub(crate) struct Inner {
        pub(crate) bytes: RefCell<Vec<u8>>,
        pub(crate) read_pos: RefCell<usize>,
        /// Largest number of bytes one `read`/`write` will transfer, to exercise the loops.
        pub(crate) chunk: usize,
        /// Makes the next call report a failure.
        pub(crate) fail: RefCell<bool>,
        /// Makes every `write` report zero bytes transferred.
        pub(crate) stall: bool,
    }

    impl FakeStream {
        pub(crate) fn new(bytes: Vec<u8>) -> Self {
            Self::with(bytes, usize::MAX, false)
        }

        pub(crate) fn with(bytes: Vec<u8>, chunk: usize, stall: bool) -> Self {
            let inner = Box::new(Inner {
                bytes: RefCell::new(bytes),
                read_pos: RefCell::new(0),
                chunk,
                fail: RefCell::new(false),
                stall,
            });
            let ctx = (&raw const *inner).cast::<c_void>().cast_mut();
            Self {
                inner,
                table: DauxStreamV1 {
                    size: DauxStreamV1::SIZE,
                    _pad0: 0,
                    ctx,
                    read: Some(read),
                    write: Some(write),
                    reserved: [0; 4],
                },
            }
        }

        pub(crate) fn read_only(bytes: Vec<u8>) -> Self {
            let mut this = Self::new(bytes);
            this.table.write = None;
            this
        }

        pub(crate) fn write_only() -> Self {
            let mut this = Self::new(Vec::new());
            this.table.read = None;
            this
        }

        pub(crate) fn failing() -> Self {
            let this = Self::new(vec![1, 2, 3]);
            *this.inner.fail.borrow_mut() = true;
            this
        }

        pub(crate) fn table(&self) -> *const DauxStreamV1 {
            &raw const self.table
        }

        pub(crate) fn written(&self) -> Vec<u8> {
            self.inner.bytes.borrow().clone()
        }
    }

    unsafe extern "C" fn read(ctx: *mut c_void, buf: *mut u8, len: usize) -> isize {
        // SAFETY: `ctx` is the `Inner` the table was built with, which outlives the table.
        let inner = unsafe { &*ctx.cast::<Inner>() };
        if *inner.fail.borrow() {
            return -1;
        }
        let bytes = inner.bytes.borrow();
        let mut pos = inner.read_pos.borrow_mut();
        let n = len.min(inner.chunk).min(bytes.len() - *pos);
        // SAFETY: the caller guarantees `buf` is writable for `len` bytes and `n <= len`.
        unsafe { core::ptr::copy_nonoverlapping(bytes[*pos..].as_ptr(), buf, n) };
        *pos += n;
        n as isize
    }

    unsafe extern "C" fn write(ctx: *mut c_void, buf: *const u8, len: usize) -> isize {
        // SAFETY: as `read`.
        let inner = unsafe { &*ctx.cast::<Inner>() };
        if *inner.fail.borrow() {
            return -1;
        }
        if inner.stall {
            return 0;
        }
        let n = len.min(inner.chunk);
        // SAFETY: the caller guarantees `buf` is readable for `len` bytes and `n <= len`.
        let slice = unsafe { core::slice::from_raw_parts(buf, n) };
        inner.bytes.borrow_mut().extend_from_slice(slice);
        n as isize
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeStream;
    use super::*;

    #[test]
    fn a_whole_stream_is_read_in_one_or_many_chunks() {
        let payload: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        let stream = FakeStream::new(payload.clone());
        // SAFETY: the stream outlives the call.
        let read = unsafe { read_all(stream.table(), usize::MAX) }.expect("readable");
        assert_eq!(read, payload);

        // Same payload, but the host dribbles it out 7 bytes at a time — which a pipe or a
        // decompressing stream really does.
        let stream = FakeStream::with(payload.clone(), 7, false);
        // SAFETY: the stream outlives the call.
        let read = unsafe { read_all(stream.table(), usize::MAX) }.expect("readable");
        assert_eq!(
            read, payload,
            "a short read must not end the transfer early"
        );
    }

    #[test]
    fn an_empty_stream_reads_as_no_bytes() {
        let stream = FakeStream::new(Vec::new());
        // SAFETY: the stream outlives the call.
        assert_eq!(unsafe { read_all(stream.table(), 64) }, Ok(Vec::new()));
    }

    #[test]
    fn a_stream_longer_than_the_limit_is_refused_before_it_is_swallowed() {
        let stream = FakeStream::with(vec![0u8; 64 * 1024], 1024, false);
        // SAFETY: the stream outlives the call.
        let result = unsafe { read_all(stream.table(), 4096) };
        assert_eq!(result, Err(DAUX_ERR_OUT_OF_MEMORY));
    }

    #[test]
    fn a_failing_or_absent_read_is_an_error_not_a_panic() {
        let stream = FakeStream::failing();
        // SAFETY: the stream outlives the call.
        assert_eq!(unsafe { read_all(stream.table(), 64) }, Err(DAUX_ERR_IO));

        let stream = FakeStream::write_only();
        // SAFETY: the stream outlives the call.
        let result = unsafe { read_all(stream.table(), 64) };
        assert_eq!(result, Err(DAUX_ERR_INVALID_ARG));

        // SAFETY: a null stream is explicitly allowed by the contract.
        let result = unsafe { read_all(core::ptr::null(), 64) };
        assert_eq!(result, Err(DAUX_ERR_INVALID_ARG));
    }

    #[test]
    fn a_short_table_is_rejected_rather_than_called() {
        let stream = FakeStream::new(vec![1, 2, 3]);
        // SAFETY: the table is a plain `Copy` structure published by the fake stream.
        let mut short = unsafe { *stream.table() };
        short.size = 8;
        // SAFETY: `short` is a live local.
        let read = unsafe { read_all(&raw const short, 64) };
        assert_eq!(read, Err(daux_abi::DAUX_ERR_ABI_MISMATCH));
        // SAFETY: as above.
        let written = unsafe { write_all(&raw const short, &[1]) };
        assert_eq!(written, Err(daux_abi::DAUX_ERR_ABI_MISMATCH));
    }

    #[test]
    fn every_byte_is_written_even_when_the_host_takes_them_a_few_at_a_time() {
        let payload: Vec<u8> = (0..5_000u32).map(|i| (i % 251) as u8).collect();
        let stream = FakeStream::with(Vec::new(), 13, false);
        // SAFETY: the stream outlives the call.
        unsafe { write_all(stream.table(), &payload) }.expect("writable");
        assert_eq!(stream.written(), payload);
    }

    #[test]
    fn a_stalling_write_reports_io_rather_than_spinning_forever() {
        let stream = FakeStream::with(Vec::new(), 64, true);
        // SAFETY: the stream outlives the call.
        let result = unsafe { write_all(stream.table(), &[1, 2, 3]) };
        assert_eq!(result, Err(DAUX_ERR_IO));

        let stream = FakeStream::read_only(Vec::new());
        // SAFETY: the stream outlives the call.
        let result = unsafe { write_all(stream.table(), &[1]) };
        assert_eq!(result, Err(DAUX_ERR_INVALID_ARG));
    }

    #[test]
    fn writing_nothing_to_a_read_only_stream_is_still_refused() {
        let stream = FakeStream::read_only(Vec::new());
        // An empty write is still a write: reporting success would let a save of an empty
        // state silently "succeed" against a stream that cannot take it.
        // SAFETY: the stream outlives the call.
        let result = unsafe { write_all(stream.table(), &[]) };
        assert_eq!(result, Err(DAUX_ERR_INVALID_ARG));
    }
}
