//! Live profile capture backends for profcast.
//!
//! The portable [`Source`] trait lives in `profcast-core`; this crate provides
//! the platform-specific implementations and a small [`Sources`] registry to
//! look them up, mirroring `profcast-formats`' `Registry`.

#[cfg(target_os = "linux")]
pub mod linux;
// The Windows backend reads x86-64 register contexts directly, so it is gated to
// that architecture; other Windows targets simply register no backend.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub mod windows;

pub use profcast_core::capture::{CaptureSpec, Source, Target};

/// A collection of capture [`Source`]s that can be listed or looked up by name.
#[derive(Default)]
pub struct Sources {
    sources: Vec<Box<dyn Source>>,
}

impl Sources {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a registry pre-populated with every backend that compiles for
    /// the current platform.
    ///
    /// Backends are registered regardless of runtime availability; callers
    /// should consult [`Source::available`] (or [`available`](Sources::available))
    /// before using one.
    #[must_use]
    pub fn with_builtins() -> Self {
        // Allows unused mut because it errors on unsupported platforms currently,
        // rather than just silently compiling but not working with a proper
        // error about why, like it should.
        #[allow(unused_mut)]
        let mut sources = Self::new();
        #[cfg(target_os = "linux")]
        sources.register(Box::new(linux::PerfSource::new()));
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        sources.register(Box::new(windows::SamplingSource::new()));
        sources
    }

    /// Adds a backend to the registry.
    pub fn register(&mut self, source: Box<dyn Source>) {
        self.sources.push(source);
    }

    /// Looks up a backend by its [`Source::name`].
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&dyn Source> {
        self.sources
            .iter()
            .map(AsRef::as_ref)
            .find(|source| source.name() == name)
    }

    /// Returns the first registered backend that reports itself
    /// [`available`](Source::available) on this host, if any.
    #[must_use]
    pub fn available(&self) -> Option<&dyn Source> {
        self.sources
            .iter()
            .map(AsRef::as_ref)
            .find(|source| source.available())
    }

    /// Iterates over every registered backend in registration order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &dyn Source> {
        self.sources.iter().map(AsRef::as_ref)
    }
}
