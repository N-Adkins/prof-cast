//! A registry of known input and output formats.
//!
//! The registry owns a set of [`InputFormat`] implementations and a set of
//! [`OutputFormat`] implementations. Input formats can be picked by explicit
//! name (e.g. a `--from folded` flag) or by probing arbitrary bytes and
//! choosing the highest-confidence match; output formats are picked by name
//! (e.g. `--to json`) or inferred from a destination file extension.

use profcast_core::format::{Confidence, InputFormat, OutputFormat, ProbeData};

use crate::{folded::FoldedFormat, json::JsonFormat, pprof::PprofFormat};

/// The outcome of a successful probe: which format matched and how strongly.
#[derive(Debug, Clone, Copy)]
pub struct Match<'a> {
    /// The [`InputFormat`] that produced the highest [`Confidence`].
    pub format: &'a dyn InputFormat,
    /// The [`Confidence`] that format reported for the probed [`ProbeData`].
    pub confidence: Confidence,
}

/// A collection of input and output formats that can be looked up,
/// auto-detected, or inferred.
#[derive(Default)]
pub struct Registry {
    formats: Vec<Box<dyn InputFormat>>,
    outputs: Vec<Box<dyn OutputFormat>>,
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
        registry.register(Box::new(PprofFormat));
        registry.register_output(Box::new(FoldedFormat));
        registry.register_output(Box::new(PprofFormat));
        registry.register_output(Box::new(JsonFormat));
        registry
    }

    /// Adds an input format to the registry.
    ///
    /// Registration order matters: when several formats report the same
    /// confidence during a probe, the one registered first wins.
    pub fn register(&mut self, format: Box<dyn InputFormat>) {
        self.formats.push(format);
    }

    /// Adds an output format to the registry.
    pub fn register_output(&mut self, output: Box<dyn OutputFormat>) {
        self.outputs.push(output);
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

    /// Iterates over every registered input format in registration order.
    pub fn formats(&self) -> impl ExactSizeIterator<Item = &dyn InputFormat> {
        self.formats.iter().map(AsRef::as_ref)
    }

    /// Looks up an output format by its [`OutputFormat::name`].
    #[must_use]
    pub fn output_by_name(&self, name: &str) -> Option<&dyn OutputFormat> {
        let found = self
            .outputs
            .iter()
            .map(AsRef::as_ref)
            .find(|output| output.name() == name);
        if found.is_none() {
            tracing::debug!(name, "no registered output format with this name");
        }
        found
    }

    /// Looks up an output format by a destination file extension (without the
    /// leading dot), e.g. `json` for `out.json`.
    #[must_use]
    pub fn output_by_extension(&self, extension: &str) -> Option<&dyn OutputFormat> {
        self.outputs
            .iter()
            .map(AsRef::as_ref)
            .find(|output| output.extensions().contains(&extension))
    }

    /// Iterates over every registered output format in registration order.
    pub fn outputs(&self) -> impl ExactSizeIterator<Item = &dyn OutputFormat> {
        self.outputs.iter().map(AsRef::as_ref)
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
        assert!(registry.by_name("pprof").is_some());
        assert!(registry.by_name("nonexistent").is_none());
    }

    #[test]
    fn probes_pprof_content() {
        use profcast_core::{
            format::WriteOptions,
            model::{Frame, FrameId, Profile, Sample, ValueKind},
        };

        let profile = Profile {
            frames: vec![Frame {
                function: Some("main".to_owned()),
                ..Frame::default()
            }],
            samples: vec![Sample {
                stack: vec![FrameId(0)],
                values: vec![1],
            }],
            value_kinds: vec![ValueKind {
                kind: "samples".to_owned(),
                unit: "count".to_owned(),
            }],
        };
        let registry = Registry::with_builtins();
        let bytes = registry
            .output_by_name("pprof")
            .unwrap()
            .write(&profile, WriteOptions::default())
            .unwrap();

        let matched = registry
            .probe(&ProbeData {
                filename: None,
                buf: &bytes,
            })
            .unwrap();
        assert_eq!(matched.format.name(), "pprof");
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
        assert_eq!(registry.outputs().len(), 0);
        let data = ProbeData {
            filename: Some("x.folded"),
            buf: b"a;b 1\n",
        };
        assert!(registry.probe(&data).is_none());
    }

    #[test]
    fn looks_up_builtin_output_by_name() {
        let registry = Registry::with_builtins();
        assert!(registry.output_by_name("json").is_some());
        assert!(registry.output_by_name("folded").is_some());
        assert!(registry.output_by_name("pprof").is_some());
        assert!(registry.output_by_name("nonexistent").is_none());
    }

    #[test]
    fn infers_output_from_extension() {
        let registry = Registry::with_builtins();
        let json = registry.output_by_extension("json");
        assert_eq!(json.map(OutputFormat::name), Some("json"));
        let folded = registry.output_by_extension("folded");
        assert_eq!(folded.map(OutputFormat::name), Some("folded"));
        let collapsed = registry.output_by_extension("collapsed");
        assert_eq!(collapsed.map(OutputFormat::name), Some("folded"));
        let pprof = registry.output_by_extension("pprof");
        assert_eq!(pprof.map(OutputFormat::name), Some("pprof"));
        // A conventional `*.pb.gz` path surfaces only the `gz` component.
        let gz = registry.output_by_extension("gz");
        assert_eq!(gz.map(OutputFormat::name), Some("pprof"));
        assert!(registry.output_by_extension("bin").is_none());
    }
}
