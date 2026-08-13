//! Planar ↔ interleaved conversion and sample-format conversion.
//!
//! DAUx is planar end to end; these helpers exist for adapters that must talk to something
//! that is not — a file reader, a resampler, a VST3 host that hands over `f64` while the
//! plug-in processes in `f32`. Each function says plainly whether it allocates.
//!
//! The `*_into` functions write into a buffer the caller already owns, allocate nothing and
//! are safe to call from the audio thread. The `*_to_vec` / `*_to_storage` functions
//! allocate and are `[main-thread]`.

use crate::buffer::{AudioBufferMut, AudioBufferRef};
use crate::error::{AudioError, AudioResult};
use crate::sample::Sample;
use crate::storage::AudioStorage;

/// Interleaves a planar buffer into a caller-provided flat slice. `[audio-thread]` — no
/// allocation.
///
/// `dst` must hold at least `channels * frames` samples; any extra tail is left untouched.
/// `dst` must not overlap `src`.
///
/// # Errors
///
/// [`AudioError::SizeMismatch`] when `dst` is too small; nothing is written in that case.
pub fn interleave_into<T: Sample>(src: &AudioBufferRef<'_, T>, dst: &mut [T]) -> AudioResult<()> {
    let channels = src.channel_count();
    let needed = src.sample_count();
    if dst.len() < needed {
        return Err(AudioError::SizeMismatch {
            expected: needed,
            found: dst.len(),
        });
    }
    for (c, channel) in src.iter().enumerate() {
        for (f, sample) in channel.iter().enumerate() {
            dst[f * channels + c] = *sample;
        }
    }
    Ok(())
}

/// De-interleaves a flat slice into a planar buffer. `[audio-thread]` — no allocation.
///
/// `src` must hold at least `channels * frames` samples; any extra tail is ignored. `src`
/// must not overlap `dst`.
///
/// # Errors
///
/// [`AudioError::SizeMismatch`] when `src` is too small; nothing is written in that case.
pub fn deinterleave_into<T: Sample>(src: &[T], dst: &mut AudioBufferMut<'_, T>) -> AudioResult<()> {
    let channels = dst.channel_count();
    let needed = dst.sample_count();
    if src.len() < needed {
        return Err(AudioError::SizeMismatch {
            expected: needed,
            found: src.len(),
        });
    }
    for (c, channel) in dst.split_channels_mut().enumerate() {
        for (f, sample) in channel.iter_mut().enumerate() {
            *sample = src[f * channels + c];
        }
    }
    Ok(())
}

/// Interleaves a planar buffer into a freshly allocated `Vec`. `[main-thread]` —
/// **allocates**.
#[must_use]
pub fn interleave_to_vec<T: Sample>(src: &AudioBufferRef<'_, T>) -> Vec<T> {
    let mut out = vec![T::ZERO; src.sample_count()];
    // The destination was sized from `src`, so this cannot fail.
    let _ = interleave_into(src, &mut out);
    out
}

/// De-interleaves a flat slice into freshly allocated planar storage. `[main-thread]` —
/// **allocates**.
///
/// # Errors
///
/// [`AudioError::ZeroChannels`] if `channels` is zero, or [`AudioError::NotDivisible`] if
/// `src.len()` is not a whole number of frames.
pub fn deinterleave_to_storage<T: Sample>(
    src: &[T],
    channels: usize,
) -> AudioResult<AudioStorage<T>> {
    AudioStorage::from_interleaved(src, channels)
}

/// Copies `src` into `dst`, converting the sample representation. `[audio-thread]` — no
/// allocation.
///
/// This is how an adapter bridges a `f64` host to a `f32` processor and back. Conversion is
/// done sample by sample through raw pointers, so no `&`/`&mut` pair is ever created over
/// the same memory.
///
/// When `S` and `D` are the same type, `src` and `dst` may address exactly the same memory
/// (the copy then degenerates to a no-op per sample); they must not *partially* overlap,
/// because the copy runs forward and would read samples it has already overwritten.
///
/// # Errors
///
/// [`AudioError::ChannelCountMismatch`] or [`AudioError::FrameCountMismatch`] when the
/// shapes differ; nothing is written in that case.
pub fn convert_into<S: Sample, D: Sample>(
    src: &AudioBufferRef<'_, S>,
    dst: &mut AudioBufferMut<'_, D>,
) -> AudioResult<()> {
    if src.channel_count() != dst.channel_count() {
        return Err(AudioError::ChannelCountMismatch {
            expected: dst.channel_count(),
            found: src.channel_count(),
        });
    }
    if src.frames() != dst.frames() {
        return Err(AudioError::FrameCountMismatch {
            expected: dst.frames(),
            found: src.frames(),
        });
    }
    let frames = dst.frames();
    if frames > 0 {
        for c in 0..dst.channel_count() {
            let (Some(from), Some(to)) = (src.channel_ptr(c), dst.channel_ptr_mut(c)) else {
                continue;
            };
            for i in 0..frames {
                // SAFETY: `c` is a valid channel of both views and `i < frames`, which both
                // views guarantee is readable/writable from the pointers they returned.
                // Each sample is read before the corresponding write, so the identical-
                // region case (in-place conversion) is well defined. Only raw pointers are
                // live, so no Rust aliasing rule applies.
                unsafe {
                    let value = from.add(i).read();
                    to.add(i).write(D::from_f64(value.to_f64()));
                }
            }
        }
    }
    dst.set_constant_mask(src.constant_mask());
    Ok(())
}

/// Interleaves a planar buffer into a flat slice while converting the representation.
/// `[audio-thread]` — no allocation.
///
/// # Errors
///
/// [`AudioError::SizeMismatch`] when `dst` is too small; nothing is written in that case.
pub fn interleave_convert_into<S: Sample, D: Sample>(
    src: &AudioBufferRef<'_, S>,
    dst: &mut [D],
) -> AudioResult<()> {
    let channels = src.channel_count();
    let needed = src.sample_count();
    if dst.len() < needed {
        return Err(AudioError::SizeMismatch {
            expected: needed,
            found: dst.len(),
        });
    }
    for (c, channel) in src.iter().enumerate() {
        for (f, sample) in channel.iter().enumerate() {
            dst[f * channels + c] = D::from_f64(sample.to_f64());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planar_stereo() -> AudioStorage<f32> {
        let mut s = AudioStorage::<f32>::new(2, 3);
        s.channel_mut(0).unwrap().copy_from_slice(&[1.0, 3.0, 5.0]);
        s.channel_mut(1).unwrap().copy_from_slice(&[2.0, 4.0, 6.0]);
        s
    }

    #[test]
    fn interleave_round_trip() {
        let s = planar_stereo();
        let mut flat = [0.0f32; 6];
        interleave_into(&s.as_ref(), &mut flat).unwrap();
        assert_eq!(flat, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(interleave_to_vec(&s.as_ref()), flat.to_vec());

        let mut back = AudioStorage::<f32>::new(2, 3);
        deinterleave_into(&flat, &mut back.as_mut()).unwrap();
        assert_eq!(back, s);

        let owned = deinterleave_to_storage(&flat, 2).unwrap();
        assert_eq!(owned, s);
    }

    #[test]
    fn interleave_tolerates_a_longer_destination() {
        let s = planar_stereo();
        let mut flat = [-1.0f32; 8];
        interleave_into(&s.as_ref(), &mut flat).unwrap();
        assert_eq!(flat, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, -1.0, -1.0]);

        let mut back = AudioStorage::<f32>::new(2, 3);
        deinterleave_into(&flat, &mut back.as_mut()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn interleave_rejects_a_short_destination() {
        let s = planar_stereo();
        let mut flat = [0.0f32; 5];
        assert_eq!(
            interleave_into(&s.as_ref(), &mut flat).unwrap_err(),
            AudioError::SizeMismatch {
                expected: 6,
                found: 5
            }
        );
        // Nothing was written.
        assert_eq!(flat, [0.0; 5]);

        let mut dst = AudioStorage::<f32>::new(2, 3);
        assert_eq!(
            deinterleave_into(&[1.0, 2.0], &mut dst.as_mut()).unwrap_err(),
            AudioError::SizeMismatch {
                expected: 6,
                found: 2
            }
        );
        assert!(dst.as_slice().iter().all(|&v| v == 0.0));
    }

    #[test]
    fn empty_conversions_are_no_ops() {
        let empty = AudioStorage::<f32>::new(0, 0);
        let mut nothing: [f32; 0] = [];
        interleave_into(&empty.as_ref(), &mut nothing).unwrap();
        assert!(interleave_to_vec(&empty.as_ref()).is_empty());

        let mut dst = AudioStorage::<f32>::new(0, 0);
        deinterleave_into(&nothing, &mut dst.as_mut()).unwrap();

        // Channels but no frames.
        let no_frames = AudioStorage::<f32>::new(4, 0);
        assert!(interleave_to_vec(&no_frames.as_ref()).is_empty());
        let mut no_frames_dst = AudioStorage::<f32>::new(4, 0);
        deinterleave_into(&nothing, &mut no_frames_dst.as_mut()).unwrap();

        // Frames but no channels.
        let no_channels = AudioStorage::<f32>::new(0, 16);
        assert!(interleave_to_vec(&no_channels.as_ref()).is_empty());
    }

    #[test]
    fn mono_interleaving_is_the_identity() {
        let mut s = AudioStorage::<f32>::new(1, 4);
        s.channel_mut(0)
            .unwrap()
            .copy_from_slice(&[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(interleave_to_vec(&s.as_ref()), vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn format_conversion_between_f32_and_f64() {
        let src = planar_stereo();
        let mut wide = AudioStorage::<f64>::new(2, 3);
        convert_into(&src.as_ref(), &mut wide.as_mut()).unwrap();
        assert_eq!(wide.channel(0).unwrap(), &[1.0, 3.0, 5.0]);
        assert_eq!(wide.channel(1).unwrap(), &[2.0, 4.0, 6.0]);

        let mut narrow = AudioStorage::<f32>::new(2, 3);
        convert_into(&wide.as_ref(), &mut narrow.as_mut()).unwrap();
        assert_eq!(narrow, src);
    }

    #[test]
    fn format_conversion_carries_the_constant_mask_and_checks_shapes() {
        let src = planar_stereo();
        let mut wide = AudioStorage::<f64>::new(2, 3);
        {
            let mut dst = wide.as_mut();
            convert_into(&src.as_ref().with_constant_mask(0b10), &mut dst).unwrap();
            assert_eq!(dst.constant_mask(), 0b10);
        }

        let mut wrong_channels = AudioStorage::<f64>::new(3, 3);
        assert_eq!(
            convert_into(&src.as_ref(), &mut wrong_channels.as_mut()).unwrap_err(),
            AudioError::ChannelCountMismatch {
                expected: 3,
                found: 2
            }
        );
        let mut wrong_frames = AudioStorage::<f64>::new(2, 4);
        assert_eq!(
            convert_into(&src.as_ref(), &mut wrong_frames.as_mut()).unwrap_err(),
            AudioError::FrameCountMismatch {
                expected: 4,
                found: 3
            }
        );
        assert!(wrong_frames.as_slice().iter().all(|&v| v == 0.0));

        // Empty shapes convert without touching anything.
        let empty = AudioStorage::<f32>::new(2, 0);
        let mut empty_dst = AudioStorage::<f64>::new(2, 0);
        convert_into(&empty.as_ref(), &mut empty_dst.as_mut()).unwrap();
    }

    #[test]
    fn conversion_saturates_instead_of_wrapping() {
        let mut huge = AudioStorage::<f64>::new(1, 3);
        huge.channel_mut(0)
            .unwrap()
            .copy_from_slice(&[f64::MAX, f64::MIN, f64::NAN]);
        let mut narrow = AudioStorage::<f32>::new(1, 3);
        convert_into(&huge.as_ref(), &mut narrow.as_mut()).unwrap();
        let out = narrow.channel(0).unwrap();
        assert!(out[0].is_infinite() && out[0] > 0.0);
        assert!(out[1].is_infinite() && out[1] < 0.0);
        assert!(out[2].is_nan());
    }

    #[test]
    fn in_place_conversion_of_the_same_region_is_a_no_op() {
        let mut s = planar_stereo();
        let expected = s.clone();
        {
            let mut dst = s.as_mut();
            let ptrs: Vec<*const f32> = (0..dst.channel_count())
                .map(|c| dst.channel_ptr_mut(c).unwrap().cast_const())
                .collect();
            // SAFETY: the pointers were just taken from `dst`, which is alive for the whole
            // block and guarantees 3 readable samples per channel; the shared view is only
            // read, and `convert_into` touches the samples through raw pointers only.
            let src = unsafe { AudioBufferRef::<f32>::from_raw(ptrs.as_ptr(), 2, 3) };
            convert_into(&src, &mut dst).unwrap();
        }
        assert_eq!(s, expected);
    }

    #[test]
    fn interleave_with_conversion() {
        let src = planar_stereo();
        let mut flat = [0.0f64; 6];
        interleave_convert_into(&src.as_ref(), &mut flat).unwrap();
        assert_eq!(flat, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);

        let mut short = [0.0f64; 1];
        assert_eq!(
            interleave_convert_into(&src.as_ref(), &mut short).unwrap_err(),
            AudioError::SizeMismatch {
                expected: 6,
                found: 1
            }
        );
        assert_eq!(short, [0.0]);
    }
}
