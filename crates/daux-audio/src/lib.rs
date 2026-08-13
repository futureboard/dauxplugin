//! Audio buffer views, channel layouts and bus topology for DAUxPlug.
//!
//! This crate owns the shape of audio in DAUx: what a sample is, how channels are laid out,
//! what a bus is, and how a block of audio is handed to a plug-in for the duration of one
//! `process` call. It has **no dependencies**, allocates in exactly three places
//! ([`AudioStorage::new`], [`BusLayout`] construction and the explicitly-marked interleave
//! helpers), and is safe to use from a real-time thread everywhere else.
//!
//! # The model
//!
//! * Audio is **planar**: one pointer per channel, `frames` contiguous samples behind each
//!   (`abi-v1` §8). Interleaved data is a foreign format that [`interleave_into`] and
//!   [`deinterleave_into`] translate at the edges.
//! * Buffers are **borrowed**. [`AudioBufferRef`] and [`AudioBufferMut`] are views over the
//!   host's memory; they never own, copy or free it, and they must not outlive the
//!   `process` call that produced them (`abi-v1` §16).
//! * Input and output buffers **may be the same memory**. In-place processing is legal and
//!   nothing in this crate assumes otherwise.
//! * The **constant mask** is a hint. Bit `c` set means channel `c` holds one value for the
//!   whole block; a zero mask means "no information", never "not constant".
//!
//! # Thread annotations
//!
//! Every public item is marked `[audio-thread]`, `[main-thread]` or `[any-thread]`,
//! matching `abi-v1` §15. Anything marked `[audio-thread]` allocates nothing, locks
//! nothing, blocks on nothing, and panics only on a programming error such as an
//! out-of-range channel index — for which a non-panicking `get_*` alternative always
//! exists.
//!
//! # Example
//!
//! ```
//! use daux_audio::{AudioStorage, ChannelLayout};
//!
//! // A host or a test allocates storage up front, off the audio thread.
//! let mut input = AudioStorage::<f32>::new(ChannelLayout::Stereo.channel_count().into(), 512);
//! let mut output = AudioStorage::<f32>::new(2, 512);
//! input.fill(0.5);
//!
//! // The audio thread only ever takes views; nothing below allocates.
//! let src = input.as_ref();
//! let mut dst = output.as_mut();
//! for (out_channel, in_channel) in dst.split_channels_mut().zip(src.iter()) {
//!     for (o, i) in out_channel.iter_mut().zip(in_channel) {
//!         *o = *i * 0.25;
//!     }
//! }
//!
//! assert_eq!(output.as_ref().sample(1, 511), Some(0.125));
//! ```

mod buffer;
mod bus;
mod buses;
mod convert;
mod error;
mod sample;
mod storage;

pub use buffer::{
    AudioBufferMut, AudioBufferRef, Channels, ChannelsMut, Frame, FrameMut, FrameSamples, Frames,
    FramesMut, view_from_slices, view_from_slices_mut,
};
pub use bus::{BusFlags, BusInfo, BusLayout, BusPurpose, ChannelLayout, layout_code, purpose_code};
pub use buses::AudioBuses;
pub use convert::{
    convert_into, deinterleave_into, deinterleave_to_storage, interleave_convert_into,
    interleave_into, interleave_to_vec,
};
pub use error::{AudioError, AudioResult};
pub use sample::{Sample, SampleFormat, SampleFormats};
pub use storage::AudioStorage;

#[cfg(test)]
mod tests {
    use super::*;

    /// A miniature end-to-end use of the crate: a host lays out buses and storage, an
    /// adapter assembles the per-block views, and a "plug-in" processes them with a
    /// sidechain and a sub-block split for sample-accurate automation.
    #[test]
    fn a_block_travels_from_host_to_plugin_and_back() {
        let layout = BusLayout::stereo_effect().with_input(
            BusInfo::new(1, "Sidechain", ChannelLayout::Mono).with_purpose(BusPurpose::Sidechain),
        );
        layout.validate().unwrap();
        assert_eq!(layout.total_input_channels(), 3);
        assert_eq!(layout.total_output_channels(), 2);

        const FRAMES: usize = 16;
        let mut main_in = AudioStorage::<f32>::new(2, FRAMES);
        let mut side_in = AudioStorage::<f32>::new(1, FRAMES);
        let mut main_out = AudioStorage::<f32>::new(2, FRAMES);
        main_in.fill(1.0);
        side_in.fill(0.5);

        {
            let inputs = [main_in.as_ref(), side_in.as_ref()];
            let mut outputs = [main_out.as_mut()];
            let mut buses = AudioBuses::new(&inputs, &mut outputs, FRAMES);

            // The "plug-in": copy the main input to the main output, then apply a gain that
            // changes halfway through the block.
            let input = buses.main_input().expect("main input");
            let sidechain_gain = buses.input(1).and_then(|b| b.sample(0, 0)).unwrap_or(1.0);
            {
                let mut out = buses.main_output().expect("main output");
                out.copy_from(&input).unwrap();

                let (mut head, mut tail) = out.split_at_frame_mut(FRAMES / 2).unwrap();
                for channel in head.split_channels_mut() {
                    for s in channel {
                        *s *= sidechain_gain;
                    }
                }
                for channel in tail.split_channels_mut() {
                    for s in channel {
                        *s *= 2.0 * sidechain_gain;
                    }
                }
            }
            // The plug-in reports that nothing is constant any more.
            buses.output_slot_mut(0).unwrap().clear_constant_mask();
        }

        let result = main_out.as_ref();
        assert_eq!(result.sample(0, 0), Some(0.5));
        assert_eq!(result.sample(1, FRAMES - 1), Some(1.0));
        assert_eq!(result.scan_constant_mask(), 0);
    }

    #[test]
    fn the_crate_surface_is_reachable_from_the_root() {
        assert_eq!(SampleFormat::F32.as_bits(), SampleFormats::F32.bits());
        assert_eq!(<f32 as Sample>::FORMAT, SampleFormat::F32);
        assert_eq!(
            ChannelLayout::from_bits(layout_code::MONO, 1).channel_count(),
            1
        );
        assert_eq!(
            BusPurpose::from_bits(purpose_code::CV),
            Some(BusPurpose::Cv)
        );
        assert!(BusFlags::IS_MAIN.contains(BusFlags::IS_MAIN));
        assert_eq!(AudioBufferRef::<f32>::empty().frames(), 0);
        assert_eq!(AudioBufferMut::<f32>::empty().frames(), 0);
        assert_eq!(AudioBuses::<f32>::empty(0).frames(), 0);
        assert!(interleave_to_vec(&AudioBufferRef::<f32>::empty()).is_empty());
        assert!(AudioError::ZeroChannels.to_string().contains("channel"));
    }
}
