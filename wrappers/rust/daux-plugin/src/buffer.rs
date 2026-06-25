//! Safe views over the realtime process data. Constructed by the export layer
//! from a raw `*const daux_process_data` for the duration of one `process()`
//! call, then handed to the plugin's `process` method.

use daux_plugin_sys as sys;

/// Transport / timeline snapshot for one process block.
#[derive(Clone, Copy, Debug)]
pub struct ProcessContext {
    pub sample_rate: f64,
    pub block_size: u32,
    pub timeline_samples: i64,
    pub tempo_bpm: f64,
    pub time_sig: (i32, i32),
    pub is_playing: bool,
    pub is_recording: bool,
    pub is_looping: bool,
}

impl ProcessContext {
    pub(crate) unsafe fn from_raw(ctx: &sys::daux_process_context) -> Self {
        ProcessContext {
            sample_rate: ctx.sample_rate,
            block_size: ctx.block_size,
            timeline_samples: ctx.timeline_samples,
            tempo_bpm: ctx.tempo_bpm,
            time_sig: (ctx.time_sig_numerator, ctx.time_sig_denominator),
            is_playing: ctx.is_playing != 0,
            is_recording: ctx.is_recording != 0,
            is_looping: ctx.is_looping != 0,
        }
    }
}

/// Safe accessor over the input/output planar float32 buffers.
///
/// All accessors are bounds-checked and return `None` for out-of-range buses /
/// channels or null pointers. Slices are valid only for the lifetime of the
/// `process()` call.
pub struct Buffers<'a> {
    data: &'a sys::daux_process_data,
    frames: usize,
}

impl<'a> Buffers<'a> {
    pub(crate) unsafe fn from_raw(data: &'a sys::daux_process_data) -> Self {
        Buffers {
            data,
            frames: data.num_frames as usize,
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }
    pub fn input_bus_count(&self) -> usize {
        self.data.input_bus_count as usize
    }
    pub fn output_bus_count(&self) -> usize {
        self.data.output_bus_count as usize
    }

    pub fn input_channels(&self, bus: usize) -> usize {
        unsafe { self.bus(self.data.inputs, self.data.input_bus_count, bus) }
            .map(|b| b.channel_count as usize)
            .unwrap_or(0)
    }
    pub fn output_channels(&self, bus: usize) -> usize {
        unsafe { self.bus(self.data.outputs, self.data.output_bus_count, bus) }
            .map(|b| b.channel_count as usize)
            .unwrap_or(0)
    }

    /// Immutable input channel slice.
    pub fn input(&self, bus: usize, ch: usize) -> Option<&[f32]> {
        unsafe {
            let b = self.bus(self.data.inputs, self.data.input_bus_count, bus)?;
            let p = self.channel_ptr(b, ch)?;
            Some(core::slice::from_raw_parts(p, self.frames))
        }
    }

    /// Mutable output channel slice.
    pub fn output(&mut self, bus: usize, ch: usize) -> Option<&mut [f32]> {
        unsafe {
            let b = self.bus(self.data.outputs, self.data.output_bus_count, bus)?;
            let p = self.channel_ptr(b, ch)?;
            Some(core::slice::from_raw_parts_mut(p, self.frames))
        }
    }

    // --- internal helpers ---
    unsafe fn bus(
        &self,
        base: *mut sys::daux_audio_bus_buffer,
        count: u32,
        idx: usize,
    ) -> Option<&sys::daux_audio_bus_buffer> {
        if base.is_null() || idx >= count as usize {
            return None;
        }
        Some(&*base.add(idx))
    }

    unsafe fn channel_ptr(&self, b: &sys::daux_audio_bus_buffer, ch: usize) -> Option<*mut f32> {
        if b.channels.is_null() || ch >= b.channel_count as usize {
            return None;
        }
        let p = *b.channels.add(ch);
        if p.is_null() {
            None
        } else {
            Some(p)
        }
    }
}
