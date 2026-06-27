//! Traits / format agnostic helpers that are used to probe
//! for different formats / obtain information about an arbitrary
//! file

use crate::{Result, model::Profile};

/// Describes how confident we are that a probe describes
/// a specific format
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// Something is completely wrong and we are positive
    /// the format does not match
    None,
    /// Generic loosely fitting signal, literally could be
    /// that the format expects UTF-8 and the passed data
    /// was UTF-8, that's the level this describes
    Weak,
    /// File extension matches and the content isn't obviously
    /// broken
    Extension,
    /// Strong match, the leading bytes obey the format's grammar
    Likely,
    /// Completely positive - stuff like magic bytes matching etc
    Certain,
}

/// This is the data passed for probing, currently is
/// just filename (potential heuristics) and
#[derive(Debug, Clone, Copy)]
pub struct ProbeData<'a> {
    /// Probed filename incase it was given (we allow
    /// arbitrary byte streams so it won't necessarily have one)
    pub filename: Option<&'a str>,
    /// Leading bytes, like a header. Could be entire file.
    pub buf: &'a [u8],
}

/// Generic trait that should be implemented for each input format.
///
/// This is the read-side mirror of [`OutputFormat`]: where an output format
/// turns a [`Profile`] into bytes, an input format turns bytes back into a
/// [`Profile`]. The input is `&[u8]` rather than `&str` so that binary formats
/// are representable, not just text.
pub trait InputFormat: Sync + Send + std::fmt::Debug {
    /// Returns a string name for the format, eg. "folded".
    fn name(&self) -> &'static str;
    /// Returns valid file extensions for this format, used for [`Confidence`]
    /// checking.
    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }
    /// Checks the passed [`ProbeData`] and returns a [`Confidence`] that the
    /// data matches this format.
    fn probe(&self, data: &ProbeData<'_>) -> Confidence;
    /// Attempts to convert arbitrary bytes into a [`Profile`]
    /// of this format
    ///
    /// # Errors
    ///
    /// Returns an error if `input` does not conform to this format's grammar
    /// (for example malformed or truncated data, or invalid text encoding).
    fn read(&self, input: &[u8]) -> Result<Profile>;
}

/// Cross-format rendering hints passed to [`OutputFormat::write`].
///
/// This is the write-side analog of [`ProbeData`]: a structured bundle of
/// options that applies to every format uniformly. The hints are best-effort -
/// a format applies the ones that are meaningful for it and ignores the rest
/// (a binary format, for instance, has no notion of pretty-printing).
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteOptions {
    /// Prefer human-readable output (indentation, whitespace) over the most
    /// compact representation. Formats with no such distinction ignore it.
    pub pretty: bool,
}

/// Generic trait that should be implemented for each output format.
///
/// This is the write-side mirror of [`InputFormat`]: where an input format
/// turns bytes into a [`Profile`], an output format turns a [`Profile`] back
/// into bytes. The output is `Vec<u8>` rather than `String` so that binary
/// formats are representable, not just text.
pub trait OutputFormat: Sync + Send + std::fmt::Debug {
    /// Returns a string name for the format, eg. "json".
    fn name(&self) -> &'static str;
    /// Returns valid file extensions for this format, used to infer the output
    /// format from a destination path (e.g. `out.json` -> `json`).
    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }
    /// Serializes `profile` into this format's byte representation, honoring the
    /// cross-format hints in `options`.
    ///
    /// # Errors
    ///
    /// Returns an error if the profile cannot be encoded, for example a
    /// serialization or I/O failure in the underlying encoder.
    fn write(&self, profile: &Profile, options: WriteOptions) -> Result<Vec<u8>>;
}
