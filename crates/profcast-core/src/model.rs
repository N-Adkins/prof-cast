//! Internal representation of a profiler's output,
//! agnostic to any specific format.

use serde::{Deserialize, Serialize};

use crate::{ProfcastError, Result};

/// Interned call frame data - we intern it because
/// fundamentally call frames will tend to appear
/// multiple times.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Frame {
    /// The raw label as it appeared
    pub raw: String,
    /// Function name
    pub function: Option<String>,
    /// File name
    pub file: Option<String>,
    /// Line number
    pub line: Option<u32>,
    /// Module type, eg. binary, .so, etc
    pub module: Option<String>,
    /// Frame memory address
    pub address: Option<u64>,
}

/// Stable index for a [`Frame`] in [`Profile::frame_intern`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameId(pub u32);

/// One aggregated stack - a path plus its weight
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    /// Root -> leaf, `stack[0]` is outermost
    pub stack: Vec<FrameId>,
    /// Parallel to [`Profile::value_kinds`], the actual data
    pub values: Vec<i64>,
}

/// [`Sample`] value metadata
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueKind {
    /// "samples", "cpu", etc
    pub kind: String,
    /// "count", "nanoseconds", "seconds", etc
    pub unit: String,
}

/// Full internal representation of a profiled output
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Profile {
    /// Interned set of [`Frame`]s
    pub frame_intern: Vec<Frame>,
    /// Profiler [`Sample`]s
    pub samples: Vec<Sample>,
    /// Profiler [`ValueKind`] dictionary
    pub value_kinds: Vec<ValueKind>,
}

impl Profile {
    /// Checks that the profile upholds the data model's structural invariants:
    /// every sample carries exactly one value per declared [`ValueKind`], and
    /// every [`FrameId`] in every stack points into [`Profile::frame_intern`].
    ///
    /// Format readers should produce valid profiles; this is a defensive check
    /// for fuzzing, tests, and untrusted inputs crossing the FFI boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ProfcastError::InvalidProfile`] describing the first violation
    /// found.
    pub fn validate(&self) -> Result<()> {
        let span = tracing::debug_span!(
            "profile.validate",
            frames = self.frame_intern.len(),
            samples = self.samples.len(),
            value_kinds = self.value_kinds.len(),
        );
        let _guard = span.enter();

        let frame_count = self.frame_intern.len();
        let value_arity = self.value_kinds.len();

        for (index, sample) in self.samples.iter().enumerate() {
            if sample.values.len() != value_arity {
                let message = format!(
                    "sample {index} has {} values but the profile declares {value_arity} value kinds",
                    sample.values.len(),
                );
                tracing::warn!(sample = index, "{message}");
                return Err(ProfcastError::InvalidProfile(message));
            }
            for frame in &sample.stack {
                if frame.0 as usize >= frame_count {
                    let message = format!(
                        "sample {index} references frame id {} but only {frame_count} frames are interned",
                        frame.0,
                    );
                    tracing::warn!(sample = index, frame = frame.0, "{message}");
                    return Err(ProfcastError::InvalidProfile(message));
                }
            }
        }

        tracing::debug!("profile is structurally valid");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_kind() -> ValueKind {
        ValueKind {
            kind: "samples".to_owned(),
            unit: "count".to_owned(),
        }
    }

    #[test]
    fn valid_profile_passes() {
        let profile = Profile {
            frame_intern: vec![Frame::default(), Frame::default()],
            samples: vec![Sample {
                stack: vec![FrameId(0), FrameId(1)],
                values: vec![1],
            }],
            value_kinds: vec![value_kind()],
        };
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn empty_profile_is_valid() {
        assert!(Profile::default().validate().is_ok());
    }

    #[test]
    fn dangling_frame_id_is_rejected() {
        let profile = Profile {
            frame_intern: vec![Frame::default()],
            samples: vec![Sample {
                stack: vec![FrameId(5)],
                values: vec![1],
            }],
            value_kinds: vec![value_kind()],
        };
        assert!(matches!(
            profile.validate(),
            Err(ProfcastError::InvalidProfile(_))
        ));
    }

    #[test]
    fn value_arity_mismatch_is_rejected() {
        let profile = Profile {
            frame_intern: vec![Frame::default()],
            samples: vec![Sample {
                stack: vec![FrameId(0)],
                values: vec![1, 2],
            }],
            value_kinds: vec![value_kind()],
        };
        assert!(matches!(
            profile.validate(),
            Err(ProfcastError::InvalidProfile(_))
        ));
    }
}
