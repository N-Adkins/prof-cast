//! Live capture sources: the read-side analog of an [`InputFormat`] that
//! sources a [`Profile`] by sampling a running system instead of parsing bytes.
//!
//! The trait and its parameters live here in the portable core; concrete
//! backends (Linux `perf_event_open`, later macOS/Windows) live in the
//! `profcast-capture` crate and are selected at runtime via
//! [`Source::available`]. Once a [`Source`] yields a [`Profile`], every
//! [`OutputFormat`] applies unchanged.
//!
//! [`InputFormat`]: crate::format::InputFormat
//! [`OutputFormat`]: crate::format::OutputFormat

use std::time::Duration;

use crate::{Result, model::Profile};

/// What a [`Source`] should profile.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Target {
    /// Profile the calling process itself. Useful for self-instrumentation and
    /// as a dependency-free smoke test of a backend.
    #[default]
    Current,
    /// Profile an already-running process by its PID. The whole process (all of
    /// its threads) is the target, not a single thread.
    Pid(u32),
    /// Launch a program and profile it from the start. The first element is the
    /// executable, the rest its arguments. The backend owns the child's
    /// lifetime: it runs to completion (or until the sampling window elapses).
    Command(Vec<String>),
}

/// How a [`Source`] should sample: what to watch, how often, and for how long.
///
/// The capture-side analog of [`ProbeData`](crate::format::ProbeData): a uniform
/// bundle every backend understands, applying what is meaningful and clamping
/// what it cannot honor exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSpec {
    /// The process to profile.
    pub target: Target,
    /// Requested sampling rate in hertz. A hint; backends may clamp it.
    pub frequency_hz: u32,
    /// How long to sample. `None` means "until the target exits" (or, for
    /// [`Target::Current`], a backend-defined default window).
    pub duration: Option<Duration>,
}

impl Default for CaptureSpec {
    fn default() -> Self {
        Self {
            target: Target::default(),
            frequency_hz: 99,
            duration: None,
        }
    }
}

/// A live producer of [`Profile`]s - the read-side analog of an
/// [`InputFormat`](crate::format::InputFormat), sourced from the running system
/// rather than from bytes.
pub trait Source: Send + Sync + std::fmt::Debug {
    /// A short, stable identifier for the backend, e.g. `"perf"`, used to
    /// select it explicitly.
    fn name(&self) -> &'static str;

    /// Whether this backend can run right now: correct platform, sufficient
    /// permissions, required kernel support present.
    ///
    /// The default returns `true`; backends that can be unsupported at runtime
    /// should override it. It must have no observable side effects beyond cheap
    /// probing.
    fn available(&self) -> bool {
        true
    }

    /// Sample the system according to `spec` and produce a [`Profile`], the same
    /// shape an [`InputFormat`] would have parsed.
    ///
    /// # Errors
    ///
    /// Returns [`ProfcastError::Unsupported`] if the backend cannot run on this
    /// host, or [`ProfcastError::Capture`] if sampling fails (permission
    /// denied, the target exited unexpectedly, a syscall error).
    ///
    /// [`InputFormat`]: crate::format::InputFormat
    /// [`ProfcastError::Unsupported`]: crate::ProfcastError::Unsupported
    /// [`ProfcastError::Capture`]: crate::ProfcastError::Capture
    fn capture(&self, spec: &CaptureSpec) -> Result<Profile>;
}
