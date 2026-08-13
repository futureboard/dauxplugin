//! The control plane: everything that is *not* audio.
//!
//! Control messages are request/response and event traffic between a host process and the
//! sandbox process that actually loads the plug-in binary. They are encoded with the
//! hand-written little-endian framing in [`crate::framing`] — no `serde`, no JSON, no
//! reflection. That is a deliberate constraint, for three reasons:
//!
//! 1. **The peer is untrusted.** A hand-written decoder has one bounds-check policy,
//!    written once, that a reader can audit end to end. A derived one has whatever the
//!    derive happens to do.
//! 2. **The layout is a compatibility contract.** A field order that a macro chooses is a
//!    field order nobody reviewed.
//! 3. **No dependencies.** `daux-protocol` depends on `daux-abi` and nothing else.
//!
//! The split from the data plane is enforced by the type system: nothing in this module
//! can carry audio, and nothing in [`crate::data`] can carry a `String` or a `Vec`.
//! Control messages are exchanged on a non-real-time thread and may allocate;
//! [`crate::data`] structures are fixed-size, pointer-free and laid out for shared memory.
//!
//! # Correlation
//!
//! Messages that expect an answer carry a [`RequestId`]. The peer answers with
//! [`ControlMessage::Ack`] (success), [`ControlMessage::StateBlob`] (the answer to
//! [`ControlMessage::SaveState`]) or [`ControlMessage::Error`], echoing the same id.
//! Unsolicited notifications — [`ControlMessage::ReportLatency`] and friends — use
//! [`RequestId::NONE`].

use daux_abi::{
    DAUX_LOG_FATAL, DAUX_PROCESS_MODE_ANALYSIS, DAUX_SAMPLE_FORMAT_F32, DAUX_SAMPLE_FORMAT_F64,
    DAUX_WINDOW_API_COCOA, DAUX_WINDOW_API_WAYLAND, DAUX_WINDOW_API_WIN32, DAUX_WINDOW_API_X11,
    DauxProcessConfigV1, DauxVersion,
};

use crate::codec::{Reader, Writer};
use crate::error::{ProtocolError, ProtocolErrorKind, ProtocolResult};
use crate::framing::{FRAME_HEADER_LEN, FrameFlags, FrameHeader};
use crate::limits::ProtocolLimits;

/// Identifies one plug-in instance inside a sandbox process. [any-thread]
///
/// Allocated by the host and never reused within the lifetime of a connection, so a
/// message that arrives after a destroy can be recognised and dropped instead of landing
/// on a fresh instance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstanceId(pub u64);

impl InstanceId {
    /// The id used by messages that concern the connection rather than an instance.
    pub const NONE: Self = Self(0);

    /// [any-thread] `true` when this id refers to an actual instance.
    #[inline]
    #[must_use]
    pub const fn is_some(self) -> bool {
        self.0 != 0
    }
}

/// Correlates a response with the request that caused it. [any-thread]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

impl RequestId {
    /// The id used by unsolicited notifications, which are never answered.
    pub const NONE: Self = Self(0);

    /// [any-thread] `true` when a response is expected for this id.
    #[inline]
    #[must_use]
    pub const fn expects_response(self) -> bool {
        self.0 != 0
    }
}

/// Which side of the connection a peer is. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PeerRole {
    /// The DAW-side process that owns the audio device and the transport.
    Host,
    /// The isolated process that loads the plug-in binary.
    Sandbox,
}

impl PeerRole {
    /// [any-thread] Wire encoding.
    #[inline]
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Host => 0,
            Self::Sandbox => 1,
        }
    }

    /// [any-thread] Decodes the wire encoding; `None` for an unassigned discriminant.
    #[inline]
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Host),
            1 => Some(Self::Sandbox),
            _ => None,
        }
    }
}

/// Optional behaviour a peer supports. [any-thread]
///
/// The handshake exchanges both sides' flags; the usable set is the
/// [intersection](FeatureFlags::intersect), which each side computes for itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FeatureFlags(pub u64);

impl FeatureFlags {
    /// No optional feature.
    pub const NONE: Self = Self(0);
    /// Audio is exchanged through a shared-memory region rather than copied over the
    /// control channel.
    pub const SHARED_MEMORY_AUDIO: Self = Self(1 << 0);
    /// The sandbox can embed its editor in a host-owned window.
    pub const EDITOR_EMBEDDING: Self = Self(1 << 1);
    /// The editor can be presented through a shared GPU texture.
    pub const SHARED_TEXTURE: Self = Self(1 << 2);
    /// MIDI 2.0 / UMP events may appear in the event stream.
    pub const MIDI2: Self = Self(1 << 3);
    /// `f64` audio buffers are supported.
    pub const DOUBLE_PRECISION: Self = Self(1 << 4);
    /// Faster-than-real-time offline rendering is supported.
    pub const OFFLINE_RENDER: Self = Self(1 << 5);

    /// [any-thread] `true` when every bit of `other` is set.
    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// [any-thread] The features both peers support.
    #[inline]
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// [any-thread] The union of two sets.
    #[inline]
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Peer identification and version negotiation. [main-thread]
///
/// Sent by both sides: the host opens with its own [`PeerRole::Host`] handshake and the
/// sandbox answers with [`PeerRole::Sandbox`] and the features it actually has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handshake {
    /// Which side sent this.
    pub role: PeerRole,
    /// Framing revision the sender speaks; see [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION).
    pub protocol_version: u16,
    /// Major DAUx ABI version the sender implements.
    pub abi_version_major: u32,
    /// Minor DAUx ABI version the sender implements.
    pub abi_version_minor: u32,
    /// Optional behaviour the sender supports.
    pub features: FeatureFlags,
    /// OS process id, for crash reporting and for a supervisor to reap the peer.
    pub process_id: u64,
    /// Build version of the sender, for diagnostics only.
    pub peer_version: DauxVersion,
    /// Human-readable sender name, for logs. Bounded by `max_string_bytes`.
    pub peer_name: String,
}

/// A failure, or the rejection of a request. [main-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorMessage {
    /// The request that failed, or [`RequestId::NONE`] for an asynchronous failure.
    pub request: RequestId,
    /// The instance concerned, or [`InstanceId::NONE`].
    pub instance: InstanceId,
    /// A `DAUX_*` status code from `docs/specifications/abi-v1.md` §2.
    pub status: i32,
    /// Human-readable detail, for logs. Bounded by `max_string_bytes`.
    pub detail: String,
}

/// Free-form diagnostics that must never influence behaviour. [main-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostics {
    /// The instance concerned, or [`InstanceId::NONE`] for process-wide diagnostics.
    pub instance: InstanceId,
    /// One of the `DAUX_LOG_*` levels.
    pub level: u32,
    /// The message. Bounded by `max_string_bytes`.
    pub text: String,
}

/// Processing configuration, mirroring [`DauxProcessConfigV1`]. [main-thread]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProcessConfigMsg {
    /// Sample rate in Hz.
    pub sample_rate: f64,
    /// Smallest block the host will ever request.
    pub min_block_size: u32,
    /// Largest block the host will ever request.
    pub max_block_size: u32,
    /// Exactly one `DAUX_SAMPLE_FORMAT_*` bit.
    pub sample_format: u32,
    /// One of the `DAUX_PROCESS_MODE_*` constants.
    pub process_mode: u32,
}

impl ProcessConfigMsg {
    /// Lowest sample rate that is not obviously a corrupt field.
    pub const MIN_SAMPLE_RATE: f64 = 1.0;
    /// Highest sample rate accepted; 64× the 48 kHz baseline, well past any real device.
    pub const MAX_SAMPLE_RATE: f64 = 3_072_000.0;

    /// [main-thread] Rejects a configuration no host could legitimately have sent.
    ///
    /// A sandbox that trusted these fields would size buffers from them, so they are
    /// checked before they reach any allocation.
    ///
    /// # Errors
    ///
    /// [`ProtocolErrorKind::InvalidValue`] for a non-finite or out-of-range sample rate, a
    /// zero or inverted block range, an unknown sample format or an unknown process mode;
    /// [`ProtocolErrorKind::LimitExceeded`] when `max_block_size` exceeds
    /// `limits.max_frames`.
    pub fn validate(&self, limits: &ProtocolLimits) -> ProtocolResult<()> {
        if !self.sample_rate.is_finite()
            || self.sample_rate < Self::MIN_SAMPLE_RATE
            || self.sample_rate > Self::MAX_SAMPLE_RATE
        {
            return Err(ProtocolError::invalid("ProcessConfig::sample_rate"));
        }
        if self.max_block_size == 0 || self.min_block_size > self.max_block_size {
            return Err(ProtocolError::invalid("ProcessConfig::block_size"));
        }
        if self.max_block_size as usize > limits.max_frames {
            return Err(ProtocolError::limit(
                "ProcessConfig::max_block_size",
                limits.max_frames,
                self.max_block_size as usize,
            ));
        }
        if self.sample_format != DAUX_SAMPLE_FORMAT_F32
            && self.sample_format != DAUX_SAMPLE_FORMAT_F64
        {
            return Err(ProtocolError::invalid("ProcessConfig::sample_format"));
        }
        if self.process_mode > DAUX_PROCESS_MODE_ANALYSIS {
            return Err(ProtocolError::invalid("ProcessConfig::process_mode"));
        }
        Ok(())
    }
}

impl From<DauxProcessConfigV1> for ProcessConfigMsg {
    #[inline]
    fn from(c: DauxProcessConfigV1) -> Self {
        Self {
            sample_rate: c.sample_rate,
            min_block_size: c.min_block_size,
            max_block_size: c.max_block_size,
            sample_format: c.sample_format,
            process_mode: c.process_mode,
        }
    }
}

impl From<ProcessConfigMsg> for DauxProcessConfigV1 {
    #[inline]
    fn from(m: ProcessConfigMsg) -> Self {
        let mut c = Self::new();
        c.sample_rate = m.sample_rate;
        c.min_block_size = m.min_block_size;
        c.max_block_size = m.max_block_size;
        c.sample_format = m.sample_format;
        c.process_mode = m.process_mode;
        c
    }
}

/// Where and how the sandbox should put its editor. [main-thread]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorWindow {
    /// One of the `DAUX_WINDOW_API_*` constants.
    pub api: u32,
    /// The parent window handle, widened to 64 bits: `HWND`, `NSView*`, an X11 `Window`
    /// or a `wl_surface*`. Zero means "create your own top-level window".
    pub handle: u64,
    /// Device pixel ratio the host is using for that window.
    pub scale: f64,
    /// Initial width in physical pixels.
    pub width: u32,
    /// Initial height in physical pixels.
    pub height: u32,
}

impl EditorWindow {
    /// [main-thread] Rejects a window description that cannot be acted on.
    ///
    /// # Errors
    ///
    /// [`ProtocolErrorKind::InvalidValue`] for an unknown windowing API, a non-finite or
    /// non-positive scale factor, or a zero dimension.
    pub fn validate(&self) -> ProtocolResult<()> {
        if !matches!(
            self.api,
            DAUX_WINDOW_API_WIN32
                | DAUX_WINDOW_API_COCOA
                | DAUX_WINDOW_API_X11
                | DAUX_WINDOW_API_WAYLAND
        ) {
            return Err(ProtocolError::invalid("EditorWindow::api"));
        }
        if !self.scale.is_finite() || self.scale <= 0.0 || self.scale > 64.0 {
            return Err(ProtocolError::invalid("EditorWindow::scale"));
        }
        if self.width == 0 || self.height == 0 {
            return Err(ProtocolError::invalid("EditorWindow::size"));
        }
        Ok(())
    }
}

/// How long a plug-in keeps producing output after its input stops. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tail {
    /// Output stops with the input.
    None,
    /// A bounded tail of this many samples.
    Samples(u32),
    /// The tail never ends; the host must decide when to stop.
    Infinite,
    /// The plug-in cannot say.
    Unknown,
}

/// Which end of a user gesture a [`ControlMessage::ParamGesture`] marks. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GesturePhase {
    /// The user grabbed the control; the host should start an automation write.
    Begin,
    /// A value changed inside an open gesture.
    Value,
    /// The user released the control.
    End,
}

impl GesturePhase {
    /// [any-thread] Wire encoding.
    #[inline]
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Begin => 0,
            Self::Value => 1,
            Self::End => 2,
        }
    }

    /// [any-thread] Decodes the wire encoding; `None` for an unassigned discriminant.
    #[inline]
    #[must_use]
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Begin),
            1 => Some(Self::Value),
            2 => Some(Self::End),
            _ => None,
        }
    }
}

/// What the plug-in needs the host to redo. [any-thread]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RestartFlags(pub u32);

impl RestartFlags {
    /// Nothing.
    pub const NONE: Self = Self(0);
    /// Deactivate and reactivate the processor.
    pub const PROCESS: Self = Self(1 << 0);
    /// Re-read the parameter list.
    pub const PARAMS: Self = Self(1 << 1);
    /// Re-read the bus layout.
    pub const PORTS: Self = Self(1 << 2);
    /// Re-read the reported latency.
    pub const LATENCY: Self = Self(1 << 3);
    /// Re-read the reported tail.
    pub const TAIL: Self = Self(1 << 4);
    /// Everything above.
    pub const ALL: Self = Self(0b1_1111);

    /// [any-thread] `true` when every bit of `other` is set.
    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// [any-thread] The union of two sets.
    #[inline]
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Discriminant of a control message, carried in the frame header. [any-thread]
///
/// The values are permanent: renumbering one silently makes two builds of DAUxPlug
/// misinterpret each other's traffic. New messages take new numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum MessageKind {
    /// [`ControlMessage::Handshake`]
    Handshake = 1,
    /// [`ControlMessage::Ack`]
    Ack = 2,
    /// [`ControlMessage::Error`]
    Error = 3,
    /// [`ControlMessage::Diagnostics`]
    Diagnostics = 4,
    /// [`ControlMessage::CreateInstance`]
    CreateInstance = 10,
    /// [`ControlMessage::DestroyInstance`]
    DestroyInstance = 11,
    /// [`ControlMessage::SetConfig`]
    SetConfig = 12,
    /// [`ControlMessage::Activate`]
    Activate = 13,
    /// [`ControlMessage::Deactivate`]
    Deactivate = 14,
    /// [`ControlMessage::SaveState`]
    SaveState = 15,
    /// [`ControlMessage::StateBlob`]
    StateBlob = 16,
    /// [`ControlMessage::LoadState`]
    LoadState = 17,
    /// [`ControlMessage::OpenEditor`]
    OpenEditor = 20,
    /// [`ControlMessage::CloseEditor`]
    CloseEditor = 21,
    /// [`ControlMessage::ResizeEditor`]
    ResizeEditor = 22,
    /// [`ControlMessage::ReportLatency`]
    ReportLatency = 30,
    /// [`ControlMessage::ReportTail`]
    ReportTail = 31,
    /// [`ControlMessage::ParamGesture`]
    ParamGesture = 32,
    /// [`ControlMessage::RequestRestart`]
    RequestRestart = 33,
}

impl MessageKind {
    /// Every kind, in wire-value order. Used by the exhaustiveness tests.
    pub const ALL: [Self; 19] = [
        Self::Handshake,
        Self::Ack,
        Self::Error,
        Self::Diagnostics,
        Self::CreateInstance,
        Self::DestroyInstance,
        Self::SetConfig,
        Self::Activate,
        Self::Deactivate,
        Self::SaveState,
        Self::StateBlob,
        Self::LoadState,
        Self::OpenEditor,
        Self::CloseEditor,
        Self::ResizeEditor,
        Self::ReportLatency,
        Self::ReportTail,
        Self::ParamGesture,
        Self::RequestRestart,
    ];

    /// [any-thread] Wire encoding.
    #[inline]
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    /// [any-thread] Decodes the wire encoding; `None` for a kind this build does not know.
    #[must_use]
    pub const fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::Handshake),
            2 => Some(Self::Ack),
            3 => Some(Self::Error),
            4 => Some(Self::Diagnostics),
            10 => Some(Self::CreateInstance),
            11 => Some(Self::DestroyInstance),
            12 => Some(Self::SetConfig),
            13 => Some(Self::Activate),
            14 => Some(Self::Deactivate),
            15 => Some(Self::SaveState),
            16 => Some(Self::StateBlob),
            17 => Some(Self::LoadState),
            20 => Some(Self::OpenEditor),
            21 => Some(Self::CloseEditor),
            22 => Some(Self::ResizeEditor),
            30 => Some(Self::ReportLatency),
            31 => Some(Self::ReportTail),
            32 => Some(Self::ParamGesture),
            33 => Some(Self::RequestRestart),
            _ => None,
        }
    }

    /// [any-thread] Stable identifier for logs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Handshake => "Handshake",
            Self::Ack => "Ack",
            Self::Error => "Error",
            Self::Diagnostics => "Diagnostics",
            Self::CreateInstance => "CreateInstance",
            Self::DestroyInstance => "DestroyInstance",
            Self::SetConfig => "SetConfig",
            Self::Activate => "Activate",
            Self::Deactivate => "Deactivate",
            Self::SaveState => "SaveState",
            Self::StateBlob => "StateBlob",
            Self::LoadState => "LoadState",
            Self::OpenEditor => "OpenEditor",
            Self::CloseEditor => "CloseEditor",
            Self::ResizeEditor => "ResizeEditor",
            Self::ReportLatency => "ReportLatency",
            Self::ReportTail => "ReportTail",
            Self::ParamGesture => "ParamGesture",
            Self::RequestRestart => "RequestRestart",
        }
    }
}

/// One control-plane message. [main-thread]
///
/// Encoded and decoded with [`ControlMessage::encode`] and [`ControlMessage::decode`];
/// see [`crate::framing`] for the frame that wraps the payload.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ControlMessage {
    /// Peer identification and version negotiation, sent by both sides.
    Handshake(Handshake),
    /// A request completed successfully.
    Ack {
        /// The request being answered.
        request: RequestId,
        /// The instance concerned, or [`InstanceId::NONE`].
        instance: InstanceId,
    },
    /// A request failed, or something went wrong asynchronously.
    Error(ErrorMessage),
    /// Free-form diagnostics.
    Diagnostics(Diagnostics),

    /// host → sandbox: load `plugin_id` from `bundle_path` as `instance`.
    CreateInstance {
        /// Correlation id.
        request: RequestId,
        /// Id the host has assigned to the new instance.
        instance: InstanceId,
        /// Reverse-DNS plug-in id inside the bundle.
        plugin_id: String,
        /// Filesystem path of the `.axt` bundle.
        bundle_path: String,
    },
    /// host → sandbox: drop the instance and free everything it owns.
    DestroyInstance {
        /// Correlation id.
        request: RequestId,
        /// The instance to destroy.
        instance: InstanceId,
    },
    /// host → sandbox: set the sample rate and block range. Sent while deactivated.
    SetConfig {
        /// Correlation id.
        request: RequestId,
        /// The instance to configure.
        instance: InstanceId,
        /// The new configuration.
        config: ProcessConfigMsg,
    },
    /// host → sandbox: prepare to process with the configuration last set.
    Activate {
        /// Correlation id.
        request: RequestId,
        /// The instance to activate.
        instance: InstanceId,
    },
    /// host → sandbox: stop processing and release per-activation resources.
    Deactivate {
        /// Correlation id.
        request: RequestId,
        /// The instance to deactivate.
        instance: InstanceId,
    },
    /// host → sandbox: serialise the instance's state. Answered with
    /// [`ControlMessage::StateBlob`].
    SaveState {
        /// Correlation id.
        request: RequestId,
        /// The instance to serialise.
        instance: InstanceId,
    },
    /// sandbox → host: the answer to a [`ControlMessage::SaveState`].
    StateBlob {
        /// The request being answered.
        request: RequestId,
        /// The instance the state belongs to.
        instance: InstanceId,
        /// Opaque `daux-state` container bytes. Bounded by `max_blob_bytes`.
        bytes: Vec<u8>,
    },
    /// host → sandbox: restore previously saved state.
    LoadState {
        /// Correlation id.
        request: RequestId,
        /// The instance to restore.
        instance: InstanceId,
        /// Opaque `daux-state` container bytes. Bounded by `max_blob_bytes`.
        bytes: Vec<u8>,
    },

    /// host → sandbox: open the editor into the given window.
    OpenEditor {
        /// Correlation id.
        request: RequestId,
        /// The instance whose editor to open.
        instance: InstanceId,
        /// Where and how to present it.
        window: EditorWindow,
    },
    /// host → sandbox: close the editor. Never touches DSP state.
    CloseEditor {
        /// Correlation id.
        request: RequestId,
        /// The instance whose editor to close.
        instance: InstanceId,
    },
    /// Either direction: the editor should be, or would like to be, this size.
    ResizeEditor {
        /// Correlation id.
        request: RequestId,
        /// The instance whose editor is resizing.
        instance: InstanceId,
        /// New width in physical pixels.
        width: u32,
        /// New height in physical pixels.
        height: u32,
    },

    /// sandbox → host: the instance's latency changed.
    ReportLatency {
        /// The instance reporting.
        instance: InstanceId,
        /// Latency in samples at the current sample rate.
        samples: u32,
    },
    /// sandbox → host: the instance's tail length changed.
    ReportTail {
        /// The instance reporting.
        instance: InstanceId,
        /// The new tail.
        tail: Tail,
    },
    /// sandbox → host: the user is driving a parameter from the plug-in's editor.
    ParamGesture {
        /// The instance reporting.
        instance: InstanceId,
        /// Permanent parameter id.
        param_id: u32,
        /// Which end of the gesture, or a value inside it.
        phase: GesturePhase,
        /// The plain (real-world) parameter value.
        value: f64,
    },
    /// sandbox → host: something changed that the host must re-read.
    RequestRestart {
        /// The instance reporting.
        instance: InstanceId,
        /// What to redo.
        flags: RestartFlags,
    },
}

impl ControlMessage {
    /// [any-thread] The discriminant written into the frame header.
    #[must_use]
    pub const fn kind(&self) -> MessageKind {
        match self {
            Self::Handshake(_) => MessageKind::Handshake,
            Self::Ack { .. } => MessageKind::Ack,
            Self::Error(_) => MessageKind::Error,
            Self::Diagnostics(_) => MessageKind::Diagnostics,
            Self::CreateInstance { .. } => MessageKind::CreateInstance,
            Self::DestroyInstance { .. } => MessageKind::DestroyInstance,
            Self::SetConfig { .. } => MessageKind::SetConfig,
            Self::Activate { .. } => MessageKind::Activate,
            Self::Deactivate { .. } => MessageKind::Deactivate,
            Self::SaveState { .. } => MessageKind::SaveState,
            Self::StateBlob { .. } => MessageKind::StateBlob,
            Self::LoadState { .. } => MessageKind::LoadState,
            Self::OpenEditor { .. } => MessageKind::OpenEditor,
            Self::CloseEditor { .. } => MessageKind::CloseEditor,
            Self::ResizeEditor { .. } => MessageKind::ResizeEditor,
            Self::ReportLatency { .. } => MessageKind::ReportLatency,
            Self::ReportTail { .. } => MessageKind::ReportTail,
            Self::ParamGesture { .. } => MessageKind::ParamGesture,
            Self::RequestRestart { .. } => MessageKind::RequestRestart,
        }
    }

    /// [any-thread] The correlation id, or [`RequestId::NONE`] for a notification.
    #[must_use]
    pub const fn request(&self) -> RequestId {
        match self {
            Self::Ack { request, .. }
            | Self::CreateInstance { request, .. }
            | Self::DestroyInstance { request, .. }
            | Self::SetConfig { request, .. }
            | Self::Activate { request, .. }
            | Self::Deactivate { request, .. }
            | Self::SaveState { request, .. }
            | Self::StateBlob { request, .. }
            | Self::LoadState { request, .. }
            | Self::OpenEditor { request, .. }
            | Self::CloseEditor { request, .. }
            | Self::ResizeEditor { request, .. } => *request,
            Self::Error(e) => e.request,
            Self::Handshake(_)
            | Self::Diagnostics(_)
            | Self::ReportLatency { .. }
            | Self::ReportTail { .. }
            | Self::ParamGesture { .. }
            | Self::RequestRestart { .. } => RequestId::NONE,
        }
    }

    /// [any-thread] The instance the message concerns, or [`InstanceId::NONE`].
    #[must_use]
    pub const fn instance(&self) -> InstanceId {
        match self {
            Self::Ack { instance, .. }
            | Self::CreateInstance { instance, .. }
            | Self::DestroyInstance { instance, .. }
            | Self::SetConfig { instance, .. }
            | Self::Activate { instance, .. }
            | Self::Deactivate { instance, .. }
            | Self::SaveState { instance, .. }
            | Self::StateBlob { instance, .. }
            | Self::LoadState { instance, .. }
            | Self::OpenEditor { instance, .. }
            | Self::CloseEditor { instance, .. }
            | Self::ResizeEditor { instance, .. }
            | Self::ReportLatency { instance, .. }
            | Self::ReportTail { instance, .. }
            | Self::ParamGesture { instance, .. }
            | Self::RequestRestart { instance, .. } => *instance,
            Self::Error(e) => e.instance,
            Self::Diagnostics(d) => d.instance,
            Self::Handshake(_) => InstanceId::NONE,
        }
    }

    /// [main-thread] Encodes the message as a complete frame.
    ///
    /// # Errors
    ///
    /// [`ProtocolErrorKind::LimitExceeded`] when a string, blob or the resulting frame
    /// exceeds `limits`. Encoding applies the same bounds as decoding, so a peer never
    /// emits a frame its counterpart is obliged to reject.
    pub fn encode(&self, limits: &ProtocolLimits) -> ProtocolResult<Vec<u8>> {
        self.encode_with_flags(FrameFlags::NONE, limits)
    }

    /// [main-thread] Encodes the message as a complete frame with explicit frame flags.
    ///
    /// # Errors
    ///
    /// As [`ControlMessage::encode`].
    pub fn encode_with_flags(
        &self,
        flags: FrameFlags,
        limits: &ProtocolLimits,
    ) -> ProtocolResult<Vec<u8>> {
        let mut out = Vec::new();
        self.encode_into(&mut out, flags, limits)?;
        Ok(out)
    }

    /// [main-thread] Appends the encoded frame to `out` and returns its length.
    ///
    /// `out` may already hold earlier frames; on failure it is truncated back to the
    /// length it had on entry, so a rejected message never leaves half a frame in a send
    /// buffer.
    ///
    /// # Errors
    ///
    /// As [`ControlMessage::encode`].
    pub fn encode_into(
        &self,
        out: &mut Vec<u8>,
        flags: FrameFlags,
        limits: &ProtocolLimits,
    ) -> ProtocolResult<usize> {
        let start = out.len();
        match self.encode_into_inner(out, start, flags, limits) {
            Ok(len) => Ok(len),
            Err(e) => {
                out.truncate(start);
                Err(e)
            }
        }
    }

    fn encode_into_inner(
        &self,
        out: &mut Vec<u8>,
        start: usize,
        flags: FrameFlags,
        limits: &ProtocolLimits,
    ) -> ProtocolResult<usize> {
        out.extend_from_slice(&[0u8; FRAME_HEADER_LEN]);
        {
            let mut w = Writer::new(out, *limits);
            self.write_payload(&mut w)?;
        }
        let payload = out
            .get(start + FRAME_HEADER_LEN..)
            .ok_or_else(|| ProtocolError::invalid("frame.payload"))?;
        let total = FRAME_HEADER_LEN + payload.len();
        if total > limits.max_frame_bytes {
            return Err(ProtocolError::limit(
                "frame.payload_len",
                limits.max_frame_bytes,
                total,
            ));
        }
        let header = FrameHeader::for_payload(self.kind().as_u16(), flags, payload)?.encode();
        out[start..start + FRAME_HEADER_LEN].copy_from_slice(&header);
        Ok(total)
    }

    /// [main-thread] Decodes a complete frame produced by [`ControlMessage::encode`].
    ///
    /// The frame is validated end to end before any field is interpreted: magic, framing
    /// version, reserved word, declared length against `limits`, the actual byte count,
    /// and the payload CRC. Only then is the payload parsed, and every field of it is
    /// bounds-checked in turn.
    ///
    /// # Errors
    ///
    /// Any [`ProtocolErrorKind`]. This function never panics and never allocates on the
    /// strength of an unvalidated length.
    pub fn decode(frame: &[u8], limits: &ProtocolLimits) -> ProtocolResult<Self> {
        let header = FrameHeader::parse(frame, limits)?;
        let payload = frame
            .get(FRAME_HEADER_LEN..)
            .ok_or_else(|| ProtocolError::truncated("frame.payload", FRAME_HEADER_LEN, frame.len()))?;
        header.verify_payload(payload)?;
        Self::decode_payload(header.kind, payload, limits)
    }

    /// [main-thread] Decodes a payload whose frame header has already been parsed and
    /// verified.
    ///
    /// Used by stream readers that have the header and the payload in hand separately.
    ///
    /// # Errors
    ///
    /// [`ProtocolErrorKind::UnknownMessage`] for a discriminant this build does not know,
    /// plus any decoding error from the payload itself.
    pub fn decode_payload(
        kind: u16,
        payload: &[u8],
        limits: &ProtocolLimits,
    ) -> ProtocolResult<Self> {
        let kind = MessageKind::from_u16(kind).ok_or(ProtocolError::new(
            ProtocolErrorKind::UnknownMessage { kind },
            "frame.kind",
        ))?;
        let mut r = Reader::new(payload);
        let message = Self::read_payload(kind, &mut r, limits)?;
        r.finish("frame.payload")?;
        Ok(message)
    }

    // One straight-line arm per message. It is long on purpose: the layout of the whole
    // control plane is visible in one screenful, next to the decoder that mirrors it.
    fn write_payload(&self, w: &mut Writer<'_>) -> ProtocolResult<()> {
        match self {
            Self::Handshake(h) => {
                w.u8(h.role.as_u8());
                w.u8(0);
                w.u16(h.protocol_version);
                w.u32(h.abi_version_major);
                w.u32(h.abi_version_minor);
                w.reserved_u32();
                w.u64(h.features.0);
                w.u64(h.process_id);
                w.u32(h.peer_version.major);
                w.u32(h.peer_version.minor);
                w.u32(h.peer_version.patch);
                w.u32(h.peer_version.build);
                w.string("Handshake::peer_name", &h.peer_name)?;
            }
            Self::Ack { request, instance } => {
                w.u64(request.0);
                w.u64(instance.0);
            }
            Self::Error(e) => {
                w.u64(e.request.0);
                w.u64(e.instance.0);
                w.i32(e.status);
                w.reserved_u32();
                w.string("Error::detail", &e.detail)?;
            }
            Self::Diagnostics(d) => {
                w.u64(d.instance.0);
                w.u32(d.level);
                w.reserved_u32();
                w.string("Diagnostics::text", &d.text)?;
            }
            Self::CreateInstance {
                request,
                instance,
                plugin_id,
                bundle_path,
            } => {
                w.u64(request.0);
                w.u64(instance.0);
                w.string("CreateInstance::plugin_id", plugin_id)?;
                w.string("CreateInstance::bundle_path", bundle_path)?;
            }
            Self::DestroyInstance { request, instance }
            | Self::Activate { request, instance }
            | Self::Deactivate { request, instance }
            | Self::SaveState { request, instance }
            | Self::CloseEditor { request, instance } => {
                w.u64(request.0);
                w.u64(instance.0);
            }
            Self::SetConfig {
                request,
                instance,
                config,
            } => {
                w.u64(request.0);
                w.u64(instance.0);
                w.f64(config.sample_rate);
                w.u32(config.min_block_size);
                w.u32(config.max_block_size);
                w.u32(config.sample_format);
                w.u32(config.process_mode);
            }
            Self::StateBlob {
                request,
                instance,
                bytes,
            }
            | Self::LoadState {
                request,
                instance,
                bytes,
            } => {
                w.u64(request.0);
                w.u64(instance.0);
                w.blob("state.bytes", bytes)?;
            }
            Self::OpenEditor {
                request,
                instance,
                window,
            } => {
                w.u64(request.0);
                w.u64(instance.0);
                w.u32(window.api);
                w.reserved_u32();
                w.u64(window.handle);
                w.f64(window.scale);
                w.u32(window.width);
                w.u32(window.height);
            }
            Self::ResizeEditor {
                request,
                instance,
                width,
                height,
            } => {
                w.u64(request.0);
                w.u64(instance.0);
                w.u32(*width);
                w.u32(*height);
            }
            Self::ReportLatency { instance, samples } => {
                w.u64(instance.0);
                w.u32(*samples);
                w.reserved_u32();
            }
            Self::ReportTail { instance, tail } => {
                w.u64(instance.0);
                let (tag, samples) = match tail {
                    Tail::None => (0u32, 0u32),
                    Tail::Samples(n) => (1, *n),
                    Tail::Infinite => (2, 0),
                    Tail::Unknown => (3, 0),
                };
                w.u32(tag);
                w.u32(samples);
            }
            Self::ParamGesture {
                instance,
                param_id,
                phase,
                value,
            } => {
                w.u64(instance.0);
                w.u32(*param_id);
                w.u32(phase.as_u32());
                w.f64(*value);
            }
            Self::RequestRestart { instance, flags } => {
                w.u64(instance.0);
                w.u32(flags.0);
                w.reserved_u32();
            }
        }
        Ok(())
    }

    // The mirror image of `write_payload`, arm for arm.
    fn read_payload(
        kind: MessageKind,
        r: &mut Reader<'_>,
        limits: &ProtocolLimits,
    ) -> ProtocolResult<Self> {
        Ok(match kind {
            MessageKind::Handshake => {
                let role = PeerRole::from_u8(r.u8("Handshake::role")?)
                    .ok_or(ProtocolError::invalid("Handshake::role"))?;
                if r.u8("Handshake::pad")? != 0 {
                    return Err(ProtocolError::invalid("Handshake::pad"));
                }
                let protocol_version = r.u16("Handshake::protocol_version")?;
                let abi_version_major = r.u32("Handshake::abi_version_major")?;
                let abi_version_minor = r.u32("Handshake::abi_version_minor")?;
                r.reserved_u32("Handshake::reserved")?;
                let features = FeatureFlags(r.u64("Handshake::features")?);
                let process_id = r.u64("Handshake::process_id")?;
                let peer_version = DauxVersion::new(
                    r.u32("Handshake::peer_version.major")?,
                    r.u32("Handshake::peer_version.minor")?,
                    r.u32("Handshake::peer_version.patch")?,
                    r.u32("Handshake::peer_version.build")?,
                );
                let peer_name = r.string("Handshake::peer_name", limits)?;
                Self::Handshake(Handshake {
                    role,
                    protocol_version,
                    abi_version_major,
                    abi_version_minor,
                    features,
                    process_id,
                    peer_version,
                    peer_name,
                })
            }
            MessageKind::Ack => Self::Ack {
                request: RequestId(r.u64("Ack::request")?),
                instance: InstanceId(r.u64("Ack::instance")?),
            },
            MessageKind::Error => {
                let request = RequestId(r.u64("Error::request")?);
                let instance = InstanceId(r.u64("Error::instance")?);
                let status = r.i32("Error::status")?;
                r.reserved_u32("Error::reserved")?;
                Self::Error(ErrorMessage {
                    request,
                    instance,
                    status,
                    detail: r.string("Error::detail", limits)?,
                })
            }
            MessageKind::Diagnostics => {
                let instance = InstanceId(r.u64("Diagnostics::instance")?);
                let level = r.u32("Diagnostics::level")?;
                if level > DAUX_LOG_FATAL {
                    return Err(ProtocolError::invalid("Diagnostics::level"));
                }
                r.reserved_u32("Diagnostics::reserved")?;
                Self::Diagnostics(Diagnostics {
                    instance,
                    level,
                    text: r.string("Diagnostics::text", limits)?,
                })
            }
            MessageKind::CreateInstance => Self::CreateInstance {
                request: RequestId(r.u64("CreateInstance::request")?),
                instance: InstanceId(r.u64("CreateInstance::instance")?),
                plugin_id: r.string("CreateInstance::plugin_id", limits)?,
                bundle_path: r.string("CreateInstance::bundle_path", limits)?,
            },
            MessageKind::DestroyInstance => Self::DestroyInstance {
                request: RequestId(r.u64("DestroyInstance::request")?),
                instance: InstanceId(r.u64("DestroyInstance::instance")?),
            },
            MessageKind::SetConfig => {
                let request = RequestId(r.u64("SetConfig::request")?);
                let instance = InstanceId(r.u64("SetConfig::instance")?);
                let config = ProcessConfigMsg {
                    sample_rate: r.f64("SetConfig::sample_rate")?,
                    min_block_size: r.u32("SetConfig::min_block_size")?,
                    max_block_size: r.u32("SetConfig::max_block_size")?,
                    sample_format: r.u32("SetConfig::sample_format")?,
                    process_mode: r.u32("SetConfig::process_mode")?,
                };
                config.validate(limits)?;
                Self::SetConfig {
                    request,
                    instance,
                    config,
                }
            }
            MessageKind::Activate => Self::Activate {
                request: RequestId(r.u64("Activate::request")?),
                instance: InstanceId(r.u64("Activate::instance")?),
            },
            MessageKind::Deactivate => Self::Deactivate {
                request: RequestId(r.u64("Deactivate::request")?),
                instance: InstanceId(r.u64("Deactivate::instance")?),
            },
            MessageKind::SaveState => Self::SaveState {
                request: RequestId(r.u64("SaveState::request")?),
                instance: InstanceId(r.u64("SaveState::instance")?),
            },
            MessageKind::StateBlob => Self::StateBlob {
                request: RequestId(r.u64("StateBlob::request")?),
                instance: InstanceId(r.u64("StateBlob::instance")?),
                bytes: r.blob("StateBlob::bytes", limits)?,
            },
            MessageKind::LoadState => Self::LoadState {
                request: RequestId(r.u64("LoadState::request")?),
                instance: InstanceId(r.u64("LoadState::instance")?),
                bytes: r.blob("LoadState::bytes", limits)?,
            },
            MessageKind::OpenEditor => {
                let request = RequestId(r.u64("OpenEditor::request")?);
                let instance = InstanceId(r.u64("OpenEditor::instance")?);
                let api = r.u32("OpenEditor::api")?;
                r.reserved_u32("OpenEditor::reserved")?;
                let window = EditorWindow {
                    api,
                    handle: r.u64("OpenEditor::handle")?,
                    scale: r.f64("OpenEditor::scale")?,
                    width: r.u32("OpenEditor::width")?,
                    height: r.u32("OpenEditor::height")?,
                };
                window.validate()?;
                Self::OpenEditor {
                    request,
                    instance,
                    window,
                }
            }
            MessageKind::CloseEditor => Self::CloseEditor {
                request: RequestId(r.u64("CloseEditor::request")?),
                instance: InstanceId(r.u64("CloseEditor::instance")?),
            },
            MessageKind::ResizeEditor => Self::ResizeEditor {
                request: RequestId(r.u64("ResizeEditor::request")?),
                instance: InstanceId(r.u64("ResizeEditor::instance")?),
                width: r.u32("ResizeEditor::width")?,
                height: r.u32("ResizeEditor::height")?,
            },
            MessageKind::ReportLatency => {
                let instance = InstanceId(r.u64("ReportLatency::instance")?);
                let samples = r.u32("ReportLatency::samples")?;
                r.reserved_u32("ReportLatency::reserved")?;
                Self::ReportLatency { instance, samples }
            }
            MessageKind::ReportTail => {
                let instance = InstanceId(r.u64("ReportTail::instance")?);
                let tag = r.u32("ReportTail::tag")?;
                let samples = r.u32("ReportTail::samples")?;
                let tail = match tag {
                    0 => Tail::None,
                    1 => Tail::Samples(samples),
                    2 => Tail::Infinite,
                    3 => Tail::Unknown,
                    _ => return Err(ProtocolError::invalid("ReportTail::tag")),
                };
                // A tag that carries no sample count must not smuggle one: otherwise two
                // encodings of the same value exist and a round-trip test proves nothing.
                if tag != 1 && samples != 0 {
                    return Err(ProtocolError::invalid("ReportTail::samples"));
                }
                Self::ReportTail { instance, tail }
            }
            MessageKind::ParamGesture => {
                let instance = InstanceId(r.u64("ParamGesture::instance")?);
                let param_id = r.u32("ParamGesture::param_id")?;
                let phase = GesturePhase::from_u32(r.u32("ParamGesture::phase")?)
                    .ok_or(ProtocolError::invalid("ParamGesture::phase"))?;
                let value = r.f64("ParamGesture::value")?;
                if !value.is_finite() {
                    return Err(ProtocolError::invalid("ParamGesture::value"));
                }
                Self::ParamGesture {
                    instance,
                    param_id,
                    phase,
                    value,
                }
            }
            MessageKind::RequestRestart => {
                let instance = InstanceId(r.u64("RequestRestart::instance")?);
                let flags = RestartFlags(r.u32("RequestRestart::flags")?);
                r.reserved_u32("RequestRestart::reserved")?;
                Self::RequestRestart { instance, flags }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ControlMessage, Diagnostics, EditorWindow, ErrorMessage, FeatureFlags, GesturePhase,
        Handshake, InstanceId, MessageKind, PeerRole, ProcessConfigMsg, RequestId, RestartFlags,
        Tail,
    };
    use crate::error::ProtocolErrorKind;
    use crate::framing::{FRAME_HEADER_LEN, FrameFlags};
    use crate::limits::ProtocolLimits;
    use daux_abi::{
        DAUX_LOG_WARN, DAUX_PROCESS_MODE_REALTIME, DAUX_SAMPLE_FORMAT_F32, DAUX_WINDOW_API_WIN32,
        DauxProcessConfigV1, DauxVersion,
    };

    fn config() -> ProcessConfigMsg {
        ProcessConfigMsg {
            sample_rate: 48_000.0,
            min_block_size: 1,
            max_block_size: 512,
            sample_format: DAUX_SAMPLE_FORMAT_F32,
            process_mode: DAUX_PROCESS_MODE_REALTIME,
        }
    }

    /// One message of every kind, used by the round-trip and corruption tests.
    fn every_message() -> Vec<ControlMessage> {
        vec![
            ControlMessage::Handshake(Handshake {
                role: PeerRole::Sandbox,
                protocol_version: 1,
                abi_version_major: 1,
                abi_version_minor: 0,
                features: FeatureFlags::SHARED_MEMORY_AUDIO.with(FeatureFlags::MIDI2),
                process_id: 4242,
                peer_version: DauxVersion::new(0, 1, 0, 7),
                peer_name: "daux-sandbox".to_owned(),
            }),
            ControlMessage::Ack {
                request: RequestId(9),
                instance: InstanceId(3),
            },
            ControlMessage::Error(ErrorMessage {
                request: RequestId(9),
                instance: InstanceId(3),
                status: -5,
                detail: "instance is not activated".to_owned(),
            }),
            ControlMessage::Diagnostics(Diagnostics {
                instance: InstanceId::NONE,
                level: DAUX_LOG_WARN,
                text: "scan took 4.2 s".to_owned(),
            }),
            ControlMessage::CreateInstance {
                request: RequestId(1),
                instance: InstanceId(1),
                plugin_id: "studio.futureboard.gain".to_owned(),
                bundle_path: "C:/plugins/Gain.axt".to_owned(),
            },
            ControlMessage::DestroyInstance {
                request: RequestId(2),
                instance: InstanceId(1),
            },
            ControlMessage::SetConfig {
                request: RequestId(3),
                instance: InstanceId(1),
                config: config(),
            },
            ControlMessage::Activate {
                request: RequestId(4),
                instance: InstanceId(1),
            },
            ControlMessage::Deactivate {
                request: RequestId(5),
                instance: InstanceId(1),
            },
            ControlMessage::SaveState {
                request: RequestId(6),
                instance: InstanceId(1),
            },
            ControlMessage::StateBlob {
                request: RequestId(6),
                instance: InstanceId(1),
                bytes: vec![0xDA, 0x00, 0xFF, 0x10],
            },
            ControlMessage::LoadState {
                request: RequestId(7),
                instance: InstanceId(1),
                bytes: Vec::new(),
            },
            ControlMessage::OpenEditor {
                request: RequestId(8),
                instance: InstanceId(1),
                window: EditorWindow {
                    api: DAUX_WINDOW_API_WIN32,
                    handle: 0xDEAD_BEEF,
                    scale: 1.5,
                    width: 640,
                    height: 480,
                },
            },
            ControlMessage::CloseEditor {
                request: RequestId(10),
                instance: InstanceId(1),
            },
            ControlMessage::ResizeEditor {
                request: RequestId(11),
                instance: InstanceId(1),
                width: 800,
                height: 600,
            },
            ControlMessage::ReportLatency {
                instance: InstanceId(1),
                samples: 64,
            },
            ControlMessage::ReportTail {
                instance: InstanceId(1),
                tail: Tail::Samples(48_000),
            },
            ControlMessage::ParamGesture {
                instance: InstanceId(1),
                param_id: 7,
                phase: GesturePhase::Begin,
                value: -6.0,
            },
            ControlMessage::RequestRestart {
                instance: InstanceId(1),
                flags: RestartFlags::PROCESS.with(RestartFlags::LATENCY),
            },
        ]
    }

    #[test]
    fn every_message_kind_has_a_sample_and_round_trips_byte_for_byte() {
        let limits = ProtocolLimits::new();
        let samples = every_message();
        assert_eq!(
            samples.len(),
            MessageKind::ALL.len(),
            "a message kind has no sample in every_message()"
        );
        for msg in samples {
            let frame = msg.encode(&limits).unwrap();
            let back = ControlMessage::decode(&frame, &limits).unwrap();
            assert_eq!(back, msg, "{:?} did not round-trip", msg.kind());
            // Re-encoding the decoded value must produce the identical bytes, which is
            // what makes the format canonical.
            assert_eq!(back.encode(&limits).unwrap(), frame);
            assert_eq!(back.kind(), msg.kind());
            assert_eq!(back.request(), msg.request());
            assert_eq!(back.instance(), msg.instance());
        }
    }

    #[test]
    fn every_tail_and_gesture_variant_round_trips() {
        let limits = ProtocolLimits::new();
        for tail in [Tail::None, Tail::Samples(0), Tail::Samples(7), Tail::Infinite, Tail::Unknown]
        {
            let msg = ControlMessage::ReportTail {
                instance: InstanceId(1),
                tail,
            };
            let frame = msg.encode(&limits).unwrap();
            assert_eq!(ControlMessage::decode(&frame, &limits).unwrap(), msg);
        }
        for phase in [GesturePhase::Begin, GesturePhase::Value, GesturePhase::End] {
            let msg = ControlMessage::ParamGesture {
                instance: InstanceId(1),
                param_id: 0,
                phase,
                value: 0.0,
            };
            let frame = msg.encode(&limits).unwrap();
            assert_eq!(ControlMessage::decode(&frame, &limits).unwrap(), msg);
            assert_eq!(GesturePhase::from_u32(phase.as_u32()), Some(phase));
        }
        assert_eq!(GesturePhase::from_u32(3), None);
        assert_eq!(PeerRole::from_u8(2), None);
    }

    #[test]
    fn message_kinds_are_a_stable_bijection() {
        for k in MessageKind::ALL {
            assert_eq!(MessageKind::from_u16(k.as_u16()), Some(k));
            assert!(!k.name().is_empty());
        }
        // Values that must never be reassigned to something else.
        assert_eq!(MessageKind::Handshake.as_u16(), 1);
        assert_eq!(MessageKind::CreateInstance.as_u16(), 10);
        assert_eq!(MessageKind::OpenEditor.as_u16(), 20);
        assert_eq!(MessageKind::RequestRestart.as_u16(), 33);
        assert_eq!(MessageKind::from_u16(0), None);
        assert_eq!(MessageKind::from_u16(9), None);
        assert_eq!(MessageKind::from_u16(u16::MAX), None);
    }

    #[test]
    fn an_unknown_message_kind_is_reported_rather_than_guessed() {
        let limits = ProtocolLimits::new();
        let mut frame = ControlMessage::Activate {
            request: RequestId(1),
            instance: InstanceId(1),
        }
        .encode(&limits)
        .unwrap();
        frame[6..8].copy_from_slice(&999u16.to_le_bytes());
        // The CRC still covers only the payload, so the frame is otherwise valid.
        let err = ControlMessage::decode(&frame, &limits).unwrap_err();
        assert_eq!(err.kind(), ProtocolErrorKind::UnknownMessage { kind: 999 });
        assert!(!err.is_fatal_to_stream(), "an unknown kind is skippable");
    }

    #[test]
    fn truncating_a_frame_anywhere_produces_an_error_and_never_a_panic() {
        let limits = ProtocolLimits::new();
        for msg in every_message() {
            let frame = msg.encode(&limits).unwrap();
            for n in 0..frame.len() {
                let err = ControlMessage::decode(&frame[..n], &limits).unwrap_err();
                // Everything short of the whole frame must fail; nothing may decode into
                // a different, plausible-looking message.
                assert!(
                    matches!(
                        err.kind(),
                        ProtocolErrorKind::Truncated { .. }
                            | ProtocolErrorKind::ChecksumMismatch { .. }
                    ),
                    "{:?} truncated to {n} bytes gave {err}",
                    msg.kind()
                );
            }
        }
    }

    #[test]
    fn flipping_any_single_bit_of_a_frame_never_yields_a_different_message() {
        let limits = ProtocolLimits::new();
        let msg = ControlMessage::CreateInstance {
            request: RequestId(1),
            instance: InstanceId(2),
            plugin_id: "studio.futureboard.gain".to_owned(),
            bundle_path: "/opt/plugins/Gain.axt".to_owned(),
        };
        let frame = msg.encode(&limits).unwrap();
        for i in 0..frame.len() {
            for bit in 0..8u32 {
                let mut corrupt = frame.clone();
                corrupt[i] ^= 1 << bit;
                // A flip inside the payload is caught by the CRC; a flip in the header is
                // caught by the magic, version, reserved word, kind or length check. The
                // two deliberate exceptions are the frame-flag bits, which are hints the
                // decoder is required to ignore, and a downgrade of the framing version,
                // which an older peer is allowed to send — neither changes the message.
                if let Ok(decoded) = ControlMessage::decode(&corrupt, &limits) {
                    assert_eq!(
                        decoded, msg,
                        "byte {i} bit {bit} corrupted into a different message"
                    );
                    assert!(
                        (8..10).contains(&i) || (4..6).contains(&i),
                        "byte {i} bit {bit} was silently tolerated outside flags/version"
                    );
                }
            }
        }
    }

    #[test]
    fn appending_garbage_to_a_frame_is_rejected() {
        let limits = ProtocolLimits::new();
        let mut frame = ControlMessage::Deactivate {
            request: RequestId(1),
            instance: InstanceId(1),
        }
        .encode(&limits)
        .unwrap();
        frame.extend_from_slice(b"junk");
        assert!(matches!(
            ControlMessage::decode(&frame, &limits).unwrap_err().kind(),
            ProtocolErrorKind::TrailingBytes { extra: 4 }
        ));
    }

    #[test]
    fn an_impossible_configuration_is_rejected_at_the_boundary() {
        let limits = ProtocolLimits::new();
        let bad = [
            ProcessConfigMsg {
                sample_rate: f64::NAN,
                ..config()
            },
            ProcessConfigMsg {
                sample_rate: 0.0,
                ..config()
            },
            ProcessConfigMsg {
                sample_rate: f64::INFINITY,
                ..config()
            },
            ProcessConfigMsg {
                max_block_size: 0,
                ..config()
            },
            ProcessConfigMsg {
                min_block_size: 513,
                ..config()
            },
            ProcessConfigMsg {
                sample_format: 0,
                ..config()
            },
            ProcessConfigMsg {
                sample_format: 3,
                ..config()
            },
            ProcessConfigMsg {
                process_mode: 4,
                ..config()
            },
        ];
        for c in bad {
            assert!(c.validate(&limits).is_err(), "{c:?} should be rejected");
            // And it must also be rejected on the wire, not only by the helper: the
            // encoder emits it, the decoder must refuse it.
            let frame = ControlMessage::SetConfig {
                request: RequestId(1),
                instance: InstanceId(1),
                config: c,
            }
            .encode(&limits)
            .unwrap();
            assert!(ControlMessage::decode(&frame, &limits).is_err());
        }
        assert!(config().validate(&limits).is_ok());
        let over = ProcessConfigMsg {
            max_block_size: 70_000,
            ..config()
        };
        assert!(matches!(
            over.validate(&limits).unwrap_err().kind(),
            ProtocolErrorKind::LimitExceeded { .. }
        ));
    }

    #[test]
    fn an_impossible_editor_window_is_rejected_at_the_boundary() {
        let limits = ProtocolLimits::new();
        let good = EditorWindow {
            api: DAUX_WINDOW_API_WIN32,
            handle: 1,
            scale: 1.0,
            width: 10,
            height: 10,
        };
        assert!(good.validate().is_ok());
        for w in [
            EditorWindow { api: 99, ..good },
            EditorWindow { api: 0, ..good },
            EditorWindow {
                scale: f64::NAN,
                ..good
            },
            EditorWindow { scale: 0.0, ..good },
            EditorWindow {
                scale: -1.0,
                ..good
            },
            EditorWindow { width: 0, ..good },
            EditorWindow { height: 0, ..good },
        ] {
            assert!(w.validate().is_err(), "{w:?} should be rejected");
            let frame = ControlMessage::OpenEditor {
                request: RequestId(1),
                instance: InstanceId(1),
                window: w,
            }
            .encode(&limits)
            .unwrap();
            assert!(ControlMessage::decode(&frame, &limits).is_err());
        }
    }

    #[test]
    fn a_blob_over_the_limit_is_refused_by_the_encoder_and_the_decoder() {
        let small = ProtocolLimits::new().with_max_blob_bytes(8);
        let generous = ProtocolLimits::new();
        let msg = ControlMessage::LoadState {
            request: RequestId(1),
            instance: InstanceId(1),
            bytes: vec![0u8; 64],
        };
        assert!(matches!(
            msg.encode(&small).unwrap_err().kind(),
            ProtocolErrorKind::LimitExceeded {
                limit: 8,
                requested: 64
            }
        ));
        let frame = msg.encode(&generous).unwrap();
        assert!(matches!(
            ControlMessage::decode(&frame, &small).unwrap_err().kind(),
            ProtocolErrorKind::LimitExceeded { .. }
        ));
    }

    #[test]
    fn a_failed_encode_leaves_the_output_buffer_untouched() {
        let limits = ProtocolLimits::new().with_max_string_bytes(4);
        let mut out = Vec::new();
        ControlMessage::Activate {
            request: RequestId(1),
            instance: InstanceId(1),
        }
        .encode_into(&mut out, FrameFlags::NONE, &limits)
        .unwrap();
        let after_first = out.clone();
        let err = ControlMessage::CreateInstance {
            request: RequestId(2),
            instance: InstanceId(2),
            plugin_id: "far too long to fit".to_owned(),
            bundle_path: String::new(),
        }
        .encode_into(&mut out, FrameFlags::NONE, &limits)
        .unwrap_err();
        assert!(matches!(err.kind(), ProtocolErrorKind::LimitExceeded { .. }));
        assert_eq!(out, after_first, "a rejected message left partial bytes");
    }

    #[test]
    fn frames_can_be_appended_back_to_back_and_carry_their_flags() {
        let limits = ProtocolLimits::new();
        let mut out = Vec::new();
        let a = ControlMessage::Activate {
            request: RequestId(1),
            instance: InstanceId(1),
        };
        let b = ControlMessage::Deactivate {
            request: RequestId(2),
            instance: InstanceId(1),
        };
        let len_a = a.encode_into(&mut out, FrameFlags::NONE, &limits).unwrap();
        let len_b = b
            .encode_into(&mut out, FrameFlags::RESPONSE, &limits)
            .unwrap();
        assert_eq!(out.len(), len_a + len_b);
        assert_eq!(ControlMessage::decode(&out[..len_a], &limits).unwrap(), a);
        assert_eq!(ControlMessage::decode(&out[len_a..], &limits).unwrap(), b);
        let header =
            crate::framing::FrameHeader::parse(&out[len_a..], &limits).unwrap();
        assert!(header.flags.contains(FrameFlags::RESPONSE));
        assert_eq!(header.frame_len(), len_b);
        assert_eq!(header.payload_len as usize, len_b - FRAME_HEADER_LEN);
    }

    #[test]
    fn a_non_finite_gesture_value_is_rejected() {
        let limits = ProtocolLimits::new();
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let frame = ControlMessage::ParamGesture {
                instance: InstanceId(1),
                param_id: 0,
                phase: GesturePhase::Value,
                value,
            }
            .encode(&limits)
            .unwrap();
            assert_eq!(
                ControlMessage::decode(&frame, &limits).unwrap_err().kind(),
                ProtocolErrorKind::InvalidValue
            );
        }
    }

    #[test]
    fn a_tail_tag_that_smuggles_a_sample_count_is_rejected() {
        let limits = ProtocolLimits::new();
        let mut frame = ControlMessage::ReportTail {
            instance: InstanceId(1),
            tail: Tail::Infinite,
        }
        .encode(&limits)
        .unwrap();
        // payload = instance(8) + tag(4) + samples(4)
        let samples_at = FRAME_HEADER_LEN + 12;
        frame[samples_at..samples_at + 4].copy_from_slice(&7u32.to_le_bytes());
        // Fix the CRC so the test exercises the semantic check, not the checksum.
        let crc = crate::framing::crc32(&frame[FRAME_HEADER_LEN..]);
        frame[16..20].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            ControlMessage::decode(&frame, &limits).unwrap_err().kind(),
            ProtocolErrorKind::InvalidValue
        );
    }

    #[test]
    fn a_diagnostics_level_outside_the_abi_range_is_rejected() {
        let limits = ProtocolLimits::new();
        let mut frame = ControlMessage::Diagnostics(Diagnostics {
            instance: InstanceId::NONE,
            level: 0,
            text: "x".to_owned(),
        })
        .encode(&limits)
        .unwrap();
        let level_at = FRAME_HEADER_LEN + 8;
        frame[level_at..level_at + 4].copy_from_slice(&6u32.to_le_bytes());
        let crc = crate::framing::crc32(&frame[FRAME_HEADER_LEN..]);
        frame[16..20].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            ControlMessage::decode(&frame, &limits).unwrap_err().kind(),
            ProtocolErrorKind::InvalidValue
        );
    }

    #[test]
    fn process_config_converts_to_and_from_the_abi_structure() {
        let msg = config();
        let abi: DauxProcessConfigV1 = msg.into();
        assert_eq!(abi.size as usize, size_of::<DauxProcessConfigV1>());
        assert_eq!(abi.reserved, [0; 6]);
        assert_eq!(ProcessConfigMsg::from(abi), msg);
    }

    #[test]
    fn feature_and_restart_flag_sets_behave_like_bitsets() {
        let both = FeatureFlags::MIDI2.with(FeatureFlags::SHARED_TEXTURE);
        assert!(both.contains(FeatureFlags::MIDI2));
        assert!(!FeatureFlags::MIDI2.contains(both));
        assert_eq!(
            both.intersect(FeatureFlags::MIDI2.with(FeatureFlags::OFFLINE_RENDER)),
            FeatureFlags::MIDI2
        );
        assert_eq!(FeatureFlags::NONE.intersect(both), FeatureFlags::NONE);
        assert!(RestartFlags::ALL.contains(RestartFlags::TAIL));
        assert!(!RestartFlags::NONE.contains(RestartFlags::PROCESS));
    }

    #[test]
    fn ids_distinguish_present_from_absent() {
        assert!(!InstanceId::NONE.is_some());
        assert!(InstanceId(1).is_some());
        assert!(!RequestId::NONE.expects_response());
        assert!(RequestId(1).expects_response());
        assert_eq!(InstanceId::default(), InstanceId::NONE);
        assert_eq!(RequestId::default(), RequestId::NONE);
    }
}
