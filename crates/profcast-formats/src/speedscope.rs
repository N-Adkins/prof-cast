//! The speedscope JSON format (`speedscope.app`).
//!
//! This is currently an output-only format. The internal model can hold several
//! value series per sample, rather than drop all but the first (as folded must
//! we emit one `sampled` profile per [`ValueKind`], all sharing the same frame
//! table, so no information is lost and the viewer can switch between series.
//!
//! Speedscope has no extension that uniquely identifies it - files conventionally
//! end in `.speedscope.json`, whose final component is the `json` already claimed
//! by [`JsonFormat`](crate::json::JsonFormat) - so it is selected by name
//! (`--to speedscope`) rather than inferred from a path.
//!
//! See <https://github.com/jlfwong/speedscope/blob/main/src/lib/file-format-spec.ts>.

use serde::Serialize;

use profcast_core::{
    Result,
    error::ProfcastError,
    format::{OutputFormat, WriteOptions},
    model::{Frame, Profile, ValueKind},
};

/// The schema URL speedscope stamps onto, and recognises in, its files.
const SCHEMA_URL: &str = "https://www.speedscope.app/file-format-schema.json";

/// The speedscope format. Stateless, so this is a zero-sized marker that
/// implements [`OutputFormat`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SpeedscopeFormat;

/// Top-level speedscope file object.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct File {
    /// The schema URL that marks this as a speedscope file.
    #[serde(rename = "$schema")]
    schema: &'static str,
    /// Tool that produced the file, surfaced in the speedscope UI.
    exporter: &'static str,
    /// Display name for the import; the model carries none, so this is fixed.
    name: &'static str,
    /// Index into `profiles` of the series shown first.
    active_profile_index: usize,
    /// The interned frame table, shared by every profile.
    shared: Shared,
    /// One `sampled` profile per value series.
    profiles: Vec<SampledProfile>,
}

/// The `shared` object: just the interned frame table.
#[derive(Serialize)]
struct Shared {
    /// Every frame referenced by any profile, indexed by position.
    frames: Vec<SpeedscopeFrame>,
}

/// One entry in the shared frame table.
///
/// `name` is required by the schema; the rest are omitted when absent so the
/// file stays compact.
#[derive(Serialize)]
struct SpeedscopeFrame {
    /// Human-readable frame label; see [`frame_name`].
    name: String,
    /// Source file the frame belongs to, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    /// 1-based source line, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
}

/// A `sampled` speedscope profile: stacks of frame indices plus parallel weights.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SampledProfile {
    /// The speedscope profile discriminant; always `sampled` here.
    #[serde(rename = "type")]
    profile_type: &'static str,
    /// The value series name, e.g. `samples` or `cpu`.
    name: String,
    /// One of speedscope's known value units; see [`speedscope_unit`].
    unit: &'static str,
    /// Lower bound of the weight range; always zero for our counts.
    start_value: i64,
    /// Upper bound of the weight range: the summed weights.
    end_value: i64,
    /// Each stack is a list of indices into `shared.frames`, root first.
    samples: Vec<Vec<usize>>,
    /// The weight of each stack in `samples`, taken from this value series.
    weights: Vec<i64>,
}

/// Maps a model [`ValueKind`] unit onto one of speedscope's recognised units.
///
/// Speedscope only understands a fixed set; anything else (notably `count`)
/// falls back to `none`, which renders weights as bare numbers.
fn speedscope_unit(unit: &str) -> &'static str {
    match unit {
        "nanoseconds" => "nanoseconds",
        "microseconds" => "microseconds",
        "milliseconds" => "milliseconds",
        "seconds" => "seconds",
        "bytes" => "bytes",
        _ => "none",
    }
}

/// Picks the speedscope display name for a [`Frame`].
///
/// Speedscope requires every frame to have a name, so this prefers the symbol
/// (function name), then the original `raw` label, then the module, then a hex
/// address, and finally a placeholder so a frame is never anonymous.
fn frame_name(frame: &Frame) -> String {
    if let Some(function) = &frame.function {
        return function.clone();
    }
    if !frame.raw.is_empty() {
        return frame.raw.clone();
    }
    if let Some(module) = &frame.module {
        return module.clone();
    }
    if let Some(address) = frame.address {
        return format!("0x{address:x}");
    }
    "unknown".to_owned()
}

/// Resolves every sample's stack to frame indices once, shared across series.
///
/// Errors if a stack references a frame id outside the interned table, matching
/// the guard the other writers apply.
fn resolve_stacks(profile: &Profile) -> Result<Vec<Vec<usize>>> {
    let frame_count = profile.frames.len();
    profile
        .samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            sample
                .stack
                .iter()
                .map(|frame_id| {
                    let position = frame_id.0 as usize;
                    if position >= frame_count {
                        return Err(ProfcastError::InvalidProfile(format!(
                            "sample {index} references frame id {} but only {frame_count} frames are interned",
                            frame_id.0,
                        )));
                    }
                    Ok(position)
                })
                .collect()
        })
        .collect()
}

/// Builds the `sampled` profile for value series `series`, reusing the shared
/// `stacks`. The weight of each sample is its value in that series.
fn build_profile(
    profile: &Profile,
    stacks: &[Vec<usize>],
    series: usize,
    kind: &ValueKind,
) -> Result<SampledProfile> {
    let mut weights = Vec::with_capacity(profile.samples.len());
    for (index, sample) in profile.samples.iter().enumerate() {
        let Some(&weight) = sample.values.get(series) else {
            return Err(ProfcastError::InvalidProfile(format!(
                "sample {index} has no value for series {series} ('{}')",
                kind.kind,
            )));
        };
        weights.push(weight);
    }

    let end_value = weights
        .iter()
        .fold(0_i64, |acc, &weight| acc.saturating_add(weight));

    Ok(SampledProfile {
        profile_type: "sampled",
        name: kind.kind.clone(),
        unit: speedscope_unit(&kind.unit),
        start_value: 0,
        end_value,
        samples: stacks.to_vec(),
        weights,
    })
}

impl OutputFormat for SpeedscopeFormat {
    fn name(&self) -> &'static str {
        "speedscope"
    }

    #[tracing::instrument(
        level = "debug",
        name = "speedscope.write",
        skip_all,
        fields(
            samples = profile.samples.len(),
            series = profile.value_kinds.len(),
        )
    )]
    fn write(&self, profile: &Profile, options: WriteOptions) -> Result<Vec<u8>> {
        let stacks = resolve_stacks(profile)?;

        let profiles = profile
            .value_kinds
            .iter()
            .enumerate()
            .map(|(series, kind)| build_profile(profile, &stacks, series, kind))
            .collect::<Result<Vec<_>>>()?;

        let frames = profile
            .frames
            .iter()
            .map(|frame| SpeedscopeFrame {
                name: frame_name(frame),
                file: frame.file.clone(),
                line: frame.line,
            })
            .collect();

        let file = File {
            schema: SCHEMA_URL,
            exporter: "profcast",
            name: "profile",
            active_profile_index: 0,
            shared: Shared { frames },
            profiles,
        };

        let bytes = if options.pretty {
            serde_json::to_vec_pretty(&file)?
        } else {
            serde_json::to_vec(&file)?
        };

        tracing::debug!(
            bytes = bytes.len(),
            profiles = file.profiles.len(),
            "wrote speedscope profile",
        );
        Ok(bytes)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use profcast_core::model::{FrameId, Sample};
    use serde_json::Value;

    use super::*;

    fn sample_profile() -> Profile {
        Profile {
            frames: vec![
                Frame {
                    function: Some("main".to_owned()),
                    file: Some("main.rs".to_owned()),
                    line: Some(42),
                    ..Frame::default()
                },
                Frame {
                    function: Some("work".to_owned()),
                    ..Frame::default()
                },
            ],
            samples: vec![
                Sample {
                    stack: vec![FrameId(0), FrameId(1)],
                    values: vec![10, 100],
                },
                Sample {
                    stack: vec![FrameId(0)],
                    values: vec![5, 50],
                },
            ],
            value_kinds: vec![
                ValueKind {
                    kind: "samples".to_owned(),
                    unit: "count".to_owned(),
                },
                ValueKind {
                    kind: "cpu".to_owned(),
                    unit: "nanoseconds".to_owned(),
                },
            ],
        }
    }

    fn write_value(profile: &Profile) -> Value {
        let bytes = SpeedscopeFormat
            .write(profile, WriteOptions::default())
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn emits_one_profile_per_value_kind() {
        let value = write_value(&sample_profile());

        assert_eq!(value["$schema"], SCHEMA_URL);
        assert_eq!(value["exporter"], "profcast");
        assert_eq!(value["activeProfileIndex"], 0);

        let profiles = value["profiles"].as_array().unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0]["type"], "sampled");
        assert_eq!(profiles[0]["name"], "samples");
        assert_eq!(profiles[1]["name"], "cpu");
    }

    #[test]
    fn shares_one_frame_table_across_series() {
        let value = write_value(&sample_profile());
        let frames = value["shared"]["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["name"], "main");
        assert_eq!(frames[0]["file"], "main.rs");
        assert_eq!(frames[0]["line"], 42);
        // `work` has no file/line, so those keys are omitted entirely.
        assert_eq!(frames[1]["name"], "work");
        assert!(frames[1].get("file").is_none());
        assert!(frames[1].get("line").is_none());
    }

    #[test]
    fn stacks_are_root_first_and_weights_track_series() {
        let value = write_value(&sample_profile());
        let cpu = &value["profiles"][1];

        // Stacks are identical across series; weights come from the cpu series.
        assert_eq!(cpu["samples"][0], serde_json::json!([0, 1]));
        assert_eq!(cpu["samples"][1], serde_json::json!([0]));
        assert_eq!(cpu["weights"], serde_json::json!([100, 50]));
        assert_eq!(cpu["unit"], "nanoseconds");
        // endValue is the summed weights of this series.
        assert_eq!(cpu["startValue"], 0);
        assert_eq!(cpu["endValue"], 150);
    }

    #[test]
    fn maps_count_unit_to_none() {
        let value = write_value(&sample_profile());
        assert_eq!(value["profiles"][0]["unit"], "none");
    }

    #[test]
    fn names_frame_from_address_when_unsymbolized() {
        let profile = Profile {
            frames: vec![Frame {
                address: Some(0x7f40_6047_5e44),
                module: Some("libc.so.6".to_owned()),
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
        let value = write_value(&profile);
        // Module beats the bare address as a display name.
        assert_eq!(value["shared"]["frames"][0]["name"], "libc.so.6");
    }

    #[test]
    fn empty_profile_has_no_profiles_or_frames() {
        let value = write_value(&Profile::default());
        assert_eq!(value["profiles"].as_array().unwrap().len(), 0);
        assert_eq!(value["shared"]["frames"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn pretty_is_multiline_compact_is_not() {
        let profile = sample_profile();
        let pretty = SpeedscopeFormat
            .write(&profile, WriteOptions { pretty: true })
            .unwrap();
        let compact = SpeedscopeFormat
            .write(&profile, WriteOptions { pretty: false })
            .unwrap();
        assert!(pretty.contains(&b'\n'));
        assert!(!compact.contains(&b'\n'));
    }

    #[test]
    fn rejects_dangling_frame_id() {
        let profile = Profile {
            frames: vec![Frame::default()],
            samples: vec![Sample {
                stack: vec![FrameId(7)],
                values: vec![1],
            }],
            value_kinds: vec![ValueKind {
                kind: "samples".to_owned(),
                unit: "count".to_owned(),
            }],
        };
        let error = SpeedscopeFormat
            .write(&profile, WriteOptions::default())
            .unwrap_err();
        assert!(matches!(error, ProfcastError::InvalidProfile(_)));
    }

    #[test]
    fn advertises_speedscope_name() {
        assert_eq!(SpeedscopeFormat.name(), "speedscope");
    }
}
