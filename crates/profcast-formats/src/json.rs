//! JSON output of the internal profile model.
//!
//! This is the canonical serialization of [`Profile`]: it writes the internal
//! model verbatim using its serde representation, so a JSON dump and the
//! in-memory profile carry exactly the same information. It doubles as the
//! default output format for the CLI and the FFI.

use profcast_core::{
    Result,
    format::{OutputFormat, WriteOptions},
    model::Profile,
};

/// Writes a [`Profile`] as JSON using the internal model's serde representation.
///
/// The format is stateless, so this is a zero-sized marker that exists only to
/// implement [`OutputFormat`].
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonFormat;

impl OutputFormat for JsonFormat {
    fn name(&self) -> &'static str {
        "json"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json"]
    }

    fn write(&self, profile: &Profile, options: WriteOptions) -> Result<Vec<u8>> {
        let bytes = if options.pretty {
            serde_json::to_vec_pretty(profile)?
        } else {
            serde_json::to_vec(profile)?
        };
        Ok(bytes)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use profcast_core::model::{Frame, FrameId, Sample, ValueKind};

    use super::*;

    fn sample_profile() -> Profile {
        Profile {
            frames: vec![Frame {
                raw: "main".to_owned(),
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
        }
    }

    #[test]
    fn round_trips_through_json() {
        let profile = sample_profile();
        let bytes = JsonFormat.write(&profile, WriteOptions::default()).unwrap();
        let decoded: Profile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(profile, decoded);
    }

    #[test]
    fn pretty_is_multiline_compact_is_not() {
        let profile = sample_profile();
        let pretty = JsonFormat
            .write(&profile, WriteOptions { pretty: true })
            .unwrap();
        let compact = JsonFormat
            .write(&profile, WriteOptions { pretty: false })
            .unwrap();
        assert!(pretty.contains(&b'\n'));
        assert!(!compact.contains(&b'\n'));
    }

    #[test]
    fn advertises_json_extension() {
        assert_eq!(JsonFormat.name(), "json");
        assert_eq!(JsonFormat.extensions(), &["json"]);
    }
}
