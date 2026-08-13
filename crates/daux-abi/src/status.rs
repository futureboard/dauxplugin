//! Status codes and the C-compatible boolean (`abi-v1` §2).

/// Result of an ABI call. `0` is success; negative values are errors.
///
/// Positive values are reserved and MUST NOT be produced by a v1 module.
///
/// [any-thread]
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DauxStatus(pub i32);

impl DauxStatus {
    /// [any-thread] Wraps a raw status code.
    #[inline]
    #[must_use]
    pub const fn from_raw(code: i32) -> Self {
        Self(code)
    }

    /// [any-thread] The raw status code.
    #[inline]
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self.0
    }

    /// [any-thread] `true` when the call succeeded.
    #[inline]
    #[must_use]
    pub const fn is_ok(self) -> bool {
        self.0 == DAUX_OK.0
    }

    /// [any-thread] `true` when the call failed.
    #[inline]
    #[must_use]
    pub const fn is_err(self) -> bool {
        self.0 < 0
    }

    /// [any-thread] Converts to a `Result`, keeping the code as the error.
    ///
    /// `Result` never crosses the ABI; this is a convenience for Rust callers on either
    /// side of the boundary.
    #[inline]
    pub const fn into_result(self) -> Result<(), DauxStatus> {
        if self.is_ok() { Ok(()) } else { Err(self) }
    }

    /// [any-thread] A short, allocation-free name for the code, for diagnostics.
    ///
    /// Unknown codes map to `"DAUX_ERR_UNKNOWN_CODE"`; this never panics and never
    /// formats, so it is safe to use from a log record built on the audio thread.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self.0 {
            0 => "DAUX_OK",
            -1 => "DAUX_ERR_UNKNOWN",
            -2 => "DAUX_ERR_INVALID_ARG",
            -3 => "DAUX_ERR_UNSUPPORTED",
            -4 => "DAUX_ERR_OUT_OF_MEMORY",
            -5 => "DAUX_ERR_INVALID_STATE",
            -6 => "DAUX_ERR_WRONG_THREAD",
            -7 => "DAUX_ERR_NOT_REALTIME",
            -8 => "DAUX_ERR_ABI_MISMATCH",
            -9 => "DAUX_ERR_VERSION",
            -10 => "DAUX_ERR_NOT_FOUND",
            -11 => "DAUX_ERR_IO",
            -12 => "DAUX_ERR_GRAPHICS",
            -13 => "DAUX_ERR_HOST",
            -14 => "DAUX_ERR_PLUGIN",
            -15 => "DAUX_ERR_PANIC",
            -16 => "DAUX_ERR_INTERNAL",
            _ => "DAUX_ERR_UNKNOWN_CODE",
        }
    }
}

impl From<i32> for DauxStatus {
    #[inline]
    fn from(code: i32) -> Self {
        Self(code)
    }
}

impl From<DauxStatus> for i32 {
    #[inline]
    fn from(status: DauxStatus) -> Self {
        status.0
    }
}

/// The call succeeded.
pub const DAUX_OK: DauxStatus = DauxStatus(0);
/// Unclassified failure.
pub const DAUX_ERR_UNKNOWN: DauxStatus = DauxStatus(-1);
/// An argument was null, out of range or otherwise malformed.
pub const DAUX_ERR_INVALID_ARG: DauxStatus = DauxStatus(-2);
/// The operation, extension or configuration is not supported.
pub const DAUX_ERR_UNSUPPORTED: DauxStatus = DauxStatus(-3);
/// A bounded allocation failed, or a bounded queue was full.
pub const DAUX_ERR_OUT_OF_MEMORY: DauxStatus = DauxStatus(-4);
/// The call is not legal in the current lifecycle state (`abi-v1` §7).
pub const DAUX_ERR_INVALID_STATE: DauxStatus = DauxStatus(-5);
/// The call was made from a thread class it is not allowed on (`abi-v1` §15).
pub const DAUX_ERR_WRONG_THREAD: DauxStatus = DauxStatus(-6);
/// The operation cannot be performed under real-time constraints.
pub const DAUX_ERR_NOT_REALTIME: DauxStatus = DauxStatus(-7);
/// Magic or major ABI version mismatch (`abi-v1` §3).
pub const DAUX_ERR_ABI_MISMATCH: DauxStatus = DauxStatus(-8);
/// Structure or state schema version is not understood.
pub const DAUX_ERR_VERSION: DauxStatus = DauxStatus(-9);
/// The named plug-in, parameter, port or resource does not exist.
pub const DAUX_ERR_NOT_FOUND: DauxStatus = DauxStatus(-10);
/// An I/O or stream operation failed.
pub const DAUX_ERR_IO: DauxStatus = DauxStatus(-11);
/// A graphics or windowing operation failed.
pub const DAUX_ERR_GRAPHICS: DauxStatus = DauxStatus(-12);
/// The host violated the contract or failed to service a request.
pub const DAUX_ERR_HOST: DauxStatus = DauxStatus(-13);
/// The plug-in violated the contract.
pub const DAUX_ERR_PLUGIN: DauxStatus = DauxStatus(-14);
/// A panic was caught at the boundary (`abi-v1` §17); the instance is poisoned.
pub const DAUX_ERR_PANIC: DauxStatus = DauxStatus(-15);
/// An internal invariant was violated.
pub const DAUX_ERR_INTERNAL: DauxStatus = DauxStatus(-16);

/// C-compatible boolean.
///
/// Producers MUST write exactly [`DAUX_FALSE`] or [`DAUX_TRUE`]; consumers MUST treat any
/// non-zero value as true.
///
/// [any-thread]
pub type DauxBool = u32;

/// The `false` value of a [`DauxBool`].
pub const DAUX_FALSE: DauxBool = 0;
/// The `true` value of a [`DauxBool`].
pub const DAUX_TRUE: DauxBool = 1;

/// [any-thread] Converts a Rust `bool` into the canonical [`DauxBool`] encoding.
#[inline]
#[must_use]
pub const fn daux_bool(value: bool) -> DauxBool {
    if value { DAUX_TRUE } else { DAUX_FALSE }
}

/// [any-thread] Interprets a [`DauxBool`] the way the specification requires: any non-zero
/// value is true.
#[inline]
#[must_use]
pub const fn daux_bool_is_true(value: DauxBool) -> bool {
    value != DAUX_FALSE
}
