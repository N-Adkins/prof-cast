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

/// Generic trait that should be implemented for each input format
pub trait InputFormat: Sync + Send + std::fmt::Debug {
    /// Returns a string name for the format, eg. "folded"
    fn name(&self) -> &'static str;
    /// Returns valid file extensions for this format, used for [`Confidence`]
    /// checking
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
