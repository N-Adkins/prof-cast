//! A registry of known input formats plus probe-based auto-detection.
//!
//! The registry owns a set of [`InputFormat`] implementations and provides two
//! ways to pick one: by explicit name (e.g. a `--from folded` flag) or by
//! probing arbitrary bytes and choosing the highest-confidence match.

use profcast_core::format::{Confidence, InputFormat, ProbeData};

use crate::folded::FoldedFormat;

/// The outcome of a successful probe: which format matched and how strongly.
#[derive(Debug, Clone, Copy)]
pub struct Match<'a> {
    /// The [`InputFormat`] that produced the highest [`Confidence`].
    pub format: &'a dyn InputFormat,
    /// The [`Confidence`] that format reported for the probed [`ProbeData`].
    pub confidence: Confidence,
}

/// A collection of input formats that can be looked up or auto-detected.
#[derive(Default)]
pub struct Registry {
    formats: Vec<Box<dyn InputFormat>>,
}

impl Registry {
    /// Creates an empty registry with no formats registered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a registry pre-populated with every format profcast ships.
    ///
    /// New built-in formats should be added here so the CLI and FFI pick them
    /// up automatically.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(FoldedFormat));
        registry
    }

    /// Adds a format to the registry.
    ///
    /// Registration order matters: when several formats report the same
    /// confidence during a probe, the one registered first wins.
    pub fn register(&mut self, format: Box<dyn InputFormat>) {
        self.formats.push(format);
    }

    /// Looks up a format by its [`InputFormat::name`].
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&dyn InputFormat> {
        let found = self
            .formats
            .iter()
            .map(AsRef::as_ref)
            .find(|format| format.name() == name);
        if found.is_none() {
            tracing::debug!(name, "no registered format with this name");
        }
        found
    }

    /// Iterates over every registered format in registration order.
    pub fn formats(&self) -> impl ExactSizeIterator<Item = &dyn InputFormat> {
        self.formats.iter().map(AsRef::as_ref)
    }

    /// Probes `data` against every format and returns the strongest match.
    ///
    /// Returns `None` if no format reports more than [`Confidence::None`]. Ties
    /// are broken in favour of the earliest-registered format.
    #[must_use]
    pub fn probe(&self, data: &ProbeData<'_>) -> Option<Match<'_>> {
        let mut best: Option<Match<'_>> = None;
        for format in self.formats.iter().map(AsRef::as_ref) {
            let confidence = format.probe(data);
            tracing::trace!(
                format = format.name(),
                ?confidence,
                "probed candidate format"
            );
            if confidence == Confidence::None {
                continue;
            }
            // Strictly greater so the first-registered format keeps ties.
            if best.is_none_or(|current| confidence > current.confidence) {
                best = Some(Match { format, confidence });
            }
        }

        if let Some(matched) = best {
            tracing::debug!(
                format = matched.format.name(),
                confidence = ?matched.confidence,
                "selected best-matching format",
            );
        } else {
            tracing::debug!("no registered format matched the input");
        }
        best
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_builtin_by_name() {
        let registry = Registry::with_builtins();
        assert!(registry.by_name("folded").is_some());
        assert!(registry.by_name("nonexistent").is_none());
    }

    #[test]
    fn probes_folded_content() {
        let registry = Registry::with_builtins();
        let data = ProbeData {
            filename: None,
            buf: b"a;b;c 10\n",
        };
        let matched = registry.probe(&data).unwrap();
        assert_eq!(matched.format.name(), "folded");
        assert_eq!(matched.confidence, Confidence::Likely);
    }

    #[test]
    fn probe_returns_none_for_garbage() {
        let registry = Registry::with_builtins();
        // Newline-terminated so the line is actually judged (and rejected: the
        // trailing token "all" is not an integer count).
        let data = ProbeData {
            filename: None,
            buf: b"definitely not a folded line at all\n",
        };
        assert!(registry.probe(&data).is_none());
    }

    #[test]
    fn empty_registry_detects_nothing() {
        let registry = Registry::new();
        assert_eq!(registry.formats().len(), 0);
        let data = ProbeData {
            filename: Some("x.folded"),
            buf: b"a;b 1\n",
        };
        assert!(registry.probe(&data).is_none());
    }
}
