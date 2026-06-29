//! The pprof profiling format (`perftools.profiles`): gzip-compressed protobuf.
//!
//! Wire types live in the private `proto` module; this module maps them to and
//! from [`Profile`].
//! Note the two orderings pprof inverts relative to the model: a sample's
//! locations are leaf-first, and an inlined location's `line[0]` is innermost.

/// Vendored prost types for the pprof wire format, generated from
/// `proto/profile.proto`. Regenerate with `just proto`; the generated file is
/// committed verbatim, so the build needs only the `prost` runtime.
mod proto {
    #![allow(
        missing_docs,
        unreachable_pub,
        clippy::all,
        clippy::pedantic,
        clippy::nursery,
        rustdoc::all
    )]
    include!("proto.gen.rs");
}

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::Path;

use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use prost::Message as _;

use profcast_core::{
    Result,
    error::ProfcastError,
    format::{Confidence, InputFormat, OutputFormat, ProbeData, WriteOptions},
    model::{Frame, FrameId, Profile, Sample, ValueKind},
};

/// Leading bytes of a gzip stream (RFC 1952). pprof payloads are conventionally
/// gzip-compressed.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Cap on decompressed bytes, so a crafted gzip bomb cannot exhaust memory.
const MAX_DECOMPRESSED: u64 = 1 << 28; // 256 MiB

/// The pprof format. Stateless, so this is a zero-sized marker that implements
/// both [`InputFormat`] and [`OutputFormat`].
#[derive(Debug, Default, Clone, Copy)]
pub struct PprofFormat;

/// Returns the raw protobuf bytes, transparently gunzipping a gzip-framed
/// input. Decompression is capped at [`MAX_DECOMPRESSED`].
fn decode_payload(input: &[u8]) -> Result<Vec<u8>> {
    if input.starts_with(&GZIP_MAGIC) {
        let mut out = Vec::new();
        GzDecoder::new(input)
            .take(MAX_DECOMPRESSED)
            .read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(input.to_vec())
    }
}

/// Resolves an `int64` index into the string table, with bounds and sign checks.
fn resolve(strings: &[String], index: i64) -> Result<String> {
    let index = usize::try_from(index)
        .map_err(|_| ProfcastError::Decode(format!("negative string index {index}")))?;
    strings.get(index).cloned().ok_or_else(|| {
        ProfcastError::Decode(format!(
            "string index {index} out of range ({} entries)",
            strings.len()
        ))
    })
}

/// Like [`resolve`], but maps the empty string (index 0 by convention) to
/// `None`, since the model distinguishes "absent" from "present but empty".
fn resolve_opt(strings: &[String], index: i64) -> Result<Option<String>> {
    let value = resolve(strings, index)?;
    Ok((!value.is_empty()).then_some(value))
}

/// Maps a pprof "unset is zero" address to the model's optional address.
fn nonzero(address: u64) -> Option<u64> {
    (address != 0).then_some(address)
}

/// Maps a pprof `int64` line number to the model's `Option<u32>`. Non-positive
/// (unset) or out-of-range values become `None`.
fn line_number(line: i64) -> Option<u32> {
    if line > 0 {
        u32::try_from(line).ok()
    } else {
        None
    }
}

/// Interns [`Frame`]s by value while reading, mirroring the model's frame table.
#[derive(Default)]
struct FrameInterner {
    frames: Vec<Frame>,
    index: HashMap<Frame, FrameId>,
}

impl FrameInterner {
    /// Returns the stable id for `frame`, interning it on first sight.
    fn intern(&mut self, frame: Frame) -> FrameId {
        if let Some(&id) = self.index.get(&frame) {
            return id;
        }
        // Frame ids are u32; saturate rather than thread a fallible path
        // through every frame (4 billion distinct frames is not a real input).
        let id = FrameId(u32::try_from(self.frames.len()).unwrap_or(u32::MAX));
        self.frames.push(frame.clone());
        self.index.insert(frame, id);
        id
    }
}

/// Expands one pprof [`Location`](proto::Location) into [`Frame`]s, innermost
/// first: one per inlined `line`, or a single address/module frame if it has
/// none (preserving stack depth).
fn expand_location(
    location: &proto::Location,
    functions: &HashMap<u64, &proto::Function>,
    mappings: &HashMap<u64, &proto::Mapping>,
    strings: &[String],
) -> Result<Vec<Frame>> {
    let module = match mappings.get(&location.mapping_id) {
        Some(mapping) => resolve_opt(strings, mapping.filename)?,
        None => None,
    };

    if location.line.is_empty() {
        return Ok(vec![Frame {
            raw: String::new(),
            function: None,
            file: None,
            line: None,
            module,
            address: nonzero(location.address),
        }]);
    }

    let mut frames = Vec::with_capacity(location.line.len());
    for (position, line) in location.line.iter().enumerate() {
        let function = functions.get(&line.function_id);
        let (name, file) = match function {
            Some(function) => (
                resolve_opt(strings, function.name)?,
                resolve_opt(strings, function.filename)?,
            ),
            None => (None, None),
        };
        frames.push(Frame {
            raw: String::new(),
            function: name,
            file,
            line: line_number(line.line),
            module: module.clone(),
            // The address belongs to the leaf frame only.
            address: (position == 0).then(|| nonzero(location.address)).flatten(),
        });
    }
    Ok(frames)
}

/// Converts a decoded pprof [`Profile`](proto::Profile) into the internal model.
fn convert_from_proto(proto: &proto::Profile) -> Result<Profile> {
    let strings = &proto.string_table;

    let value_kinds = proto
        .sample_type
        .iter()
        .map(|value_type| {
            Ok(ValueKind {
                kind: resolve(strings, value_type.r#type)?,
                unit: resolve(strings, value_type.unit)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let functions: HashMap<u64, &proto::Function> =
        proto.function.iter().map(|f| (f.id, f)).collect();
    let mappings: HashMap<u64, &proto::Mapping> = proto.mapping.iter().map(|m| (m.id, m)).collect();

    // Intern every location in declaration order so frame ids are deterministic
    // and a writer's output round-trips. `loc_frames` is innermost-first.
    let mut interner = FrameInterner::default();
    let mut loc_frames: HashMap<u64, Vec<FrameId>> = HashMap::new();
    for location in &proto.location {
        if loc_frames.contains_key(&location.id) {
            continue;
        }
        let ids = expand_location(location, &functions, &mappings, strings)?
            .into_iter()
            .map(|frame| interner.intern(frame))
            .collect();
        loc_frames.insert(location.id, ids);
    }

    let value_arity = value_kinds.len();
    let mut samples = Vec::with_capacity(proto.sample.len());
    for sample in &proto.sample {
        if sample.value.len() != value_arity {
            return Err(ProfcastError::Decode(format!(
                "sample has {} values but the profile declares {value_arity} value kinds",
                sample.value.len(),
            )));
        }

        // Gather leaf-first, then reverse to the model's root-first stack.
        let mut leaf_to_root: Vec<FrameId> = Vec::new();
        for location_id in &sample.location_id {
            let ids = loc_frames.get(location_id).ok_or_else(|| {
                ProfcastError::Decode(format!(
                    "sample references unknown location id {location_id}"
                ))
            })?;
            leaf_to_root.extend_from_slice(ids);
        }
        leaf_to_root.reverse();

        samples.push(Sample {
            stack: leaf_to_root,
            values: sample.value.clone(),
        });
    }

    Ok(Profile {
        frames: interner.frames,
        samples,
        value_kinds,
    })
}

/// Builds a pprof `string_table`, interning each string to its `int64` index.
/// Index 0 is always the empty string, as the format requires.
#[derive(Default)]
struct StringTable {
    strings: Vec<String>,
    index: HashMap<String, i64>,
}

impl StringTable {
    fn new() -> Self {
        let mut table = Self::default();
        table.strings.push(String::new());
        table.index.insert(String::new(), 0);
        table
    }

    fn intern(&mut self, value: &str) -> i64 {
        if let Some(&existing) = self.index.get(value) {
            return existing;
        }
        let id = i64::try_from(self.strings.len()).unwrap_or(i64::MAX);
        self.strings.push(value.to_owned());
        self.index.insert(value.to_owned(), id);
        id
    }

    fn into_vec(self) -> Vec<String> {
        self.strings
    }
}

/// The pprof id (location or function) for the interned frame at `index`.
/// Ids are 1-based: zero means "unset" in the format.
fn frame_id_to_pprof(index: usize) -> u64 {
    u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1)
}

/// Returns the mapping id for `module`, creating and interning a
/// [`Mapping`](proto::Mapping) on first sight.
fn intern_mapping(
    module: &str,
    ids: &mut HashMap<String, u64>,
    mappings: &mut Vec<proto::Mapping>,
    strings: &mut StringTable,
) -> u64 {
    if let Some(&id) = ids.get(module) {
        return id;
    }
    let id = frame_id_to_pprof(mappings.len());
    mappings.push(proto::Mapping {
        id,
        filename: strings.intern(module),
        ..Default::default()
    });
    ids.insert(module.to_owned(), id);
    id
}

/// Converts the internal model into a pprof [`Profile`](proto::Profile).
///
/// Each interned [`Frame`] becomes one [`Location`](proto::Location) at id
/// `index + 1`, plus a function/line when it has symbol info and a deduplicated
/// mapping per module. The 1:1 layout is what makes the round trip exact.
fn convert_to_proto(profile: &Profile) -> proto::Profile {
    let mut strings = StringTable::new();

    let sample_type = profile
        .value_kinds
        .iter()
        .map(|kind| proto::ValueType {
            r#type: strings.intern(&kind.kind),
            unit: strings.intern(&kind.unit),
        })
        .collect();

    let mut mapping_ids: HashMap<String, u64> = HashMap::new();
    let mut mappings: Vec<proto::Mapping> = Vec::new();
    let mut functions: Vec<proto::Function> = Vec::new();
    let mut locations: Vec<proto::Location> = Vec::with_capacity(profile.frames.len());

    for (index, frame) in profile.frames.iter().enumerate() {
        let id = frame_id_to_pprof(index);

        let mapping_id = frame.module.as_ref().map_or(0, |module| {
            intern_mapping(module, &mut mapping_ids, &mut mappings, &mut strings)
        });

        let lines = if frame.function.is_some() || frame.file.is_some() || frame.line.is_some() {
            functions.push(proto::Function {
                id,
                name: strings.intern(frame.function.as_deref().unwrap_or_default()),
                filename: strings.intern(frame.file.as_deref().unwrap_or_default()),
                ..Default::default()
            });
            vec![proto::Line {
                function_id: id,
                line: i64::from(frame.line.unwrap_or(0)),
                ..Default::default()
            }]
        } else {
            Vec::new()
        };

        locations.push(proto::Location {
            id,
            mapping_id,
            address: frame.address.unwrap_or(0),
            line: lines,
            ..Default::default()
        });
    }

    let sample = profile
        .samples
        .iter()
        .map(|sample| proto::Sample {
            // Model stacks are root-first; pprof wants leaf-first.
            location_id: sample
                .stack
                .iter()
                .rev()
                .map(|frame| u64::from(frame.0).saturating_add(1))
                .collect(),
            value: sample.values.clone(),
            ..Default::default()
        })
        .collect();

    proto::Profile {
        sample_type,
        sample,
        mapping: mappings,
        location: locations,
        function: functions,
        string_table: strings.into_vec(),
        ..Default::default()
    }
}

/// Encodes the model as gzip-compressed pprof protobuf.
fn write_profile(profile: &Profile) -> Result<Vec<u8>> {
    let proto = convert_to_proto(profile);
    let raw = proto.encode_to_vec();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw)?;
    Ok(encoder.finish()?)
}

/// How strongly `buf` resembles pprof: it must decode and pass
/// [`looks_like_pprof`].
fn probe_content(buf: &[u8]) -> Confidence {
    let Ok(bytes) = decode_payload(buf) else {
        return Confidence::None;
    };
    match proto::Profile::decode(bytes.as_slice()) {
        Ok(proto) if looks_like_pprof(&proto) => Confidence::Likely,
        _ => Confidence::None,
    }
}

/// Whether a decoded message has pprof's hallmarks (empty string at index 0
/// plus some structure), rather than being decoded from unrelated bytes.
fn looks_like_pprof(proto: &proto::Profile) -> bool {
    let empty_first = proto.string_table.first().is_some_and(String::is_empty);
    let has_structure =
        !proto.sample_type.is_empty() || !proto.sample.is_empty() || proto.string_table.len() > 1;
    empty_first && has_structure
}

impl InputFormat for PprofFormat {
    fn name(&self) -> &'static str {
        "pprof"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pprof", "pb"]
    }

    fn probe(&self, data: &ProbeData<'_>) -> Confidence {
        let mut confidence = probe_content(data.buf);

        let correct_extension = data
            .filename
            .map(Path::new)
            .and_then(Path::extension)
            .is_some_and(|ext| {
                InputFormat::extensions(self)
                    .iter()
                    .any(|expected| ext == *expected)
            });
        if correct_extension {
            confidence = confidence.max(Confidence::Extension);
        }

        tracing::trace!(
            filename = ?data.filename,
            bytes = data.buf.len(),
            ?confidence,
            "probed pprof format",
        );
        confidence
    }

    #[tracing::instrument(
        level = "debug",
        name = "pprof.read",
        skip_all,
        fields(bytes = input.len())
    )]
    fn read(&self, input: &[u8]) -> Result<Profile> {
        let bytes = decode_payload(input)?;
        let proto = proto::Profile::decode(bytes.as_slice())
            .map_err(|error| ProfcastError::Decode(format!("invalid pprof protobuf: {error}")))?;

        let profile = convert_from_proto(&proto)?;
        tracing::debug!(
            samples = profile.samples.len(),
            frames = profile.frames.len(),
            "parsed pprof profile",
        );
        Ok(profile)
    }
}

impl OutputFormat for PprofFormat {
    fn name(&self) -> &'static str {
        "pprof"
    }

    // `gz` is included so a conventional `*.pb.gz` path infers pprof: the CLI
    // only sees the final `.gz` component, and pprof is the only gzipped output
    // profcast produces. (Input detection relies on content probing instead.)
    fn extensions(&self) -> &'static [&'static str] {
        &["pprof", "pb", "gz"]
    }

    #[tracing::instrument(
        level = "debug",
        name = "pprof.write",
        skip_all,
        fields(samples = profile.samples.len())
    )]
    fn write(&self, profile: &Profile, _options: WriteOptions) -> Result<Vec<u8>> {
        write_profile(profile)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
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
                    module: Some("libwork.so".to_owned()),
                    ..Frame::default()
                },
                Frame {
                    address: Some(0x7f40_6047_5e44),
                    module: Some("libc.so.6".to_owned()),
                    ..Frame::default()
                },
            ],
            samples: vec![
                Sample {
                    stack: vec![FrameId(0), FrameId(1)],
                    values: vec![10, 100],
                },
                Sample {
                    stack: vec![FrameId(0), FrameId(2)],
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

    #[test]
    fn round_trips_through_pprof() {
        let profile = sample_profile();
        let bytes = PprofFormat
            .write(&profile, WriteOptions::default())
            .unwrap();
        let parsed = PprofFormat.read(&bytes).unwrap();
        assert_eq!(parsed, profile);
        parsed.validate().unwrap();
    }

    #[test]
    fn output_is_gzip_framed() {
        let bytes = PprofFormat
            .write(&sample_profile(), WriteOptions::default())
            .unwrap();
        assert_eq!(&bytes[..2], &GZIP_MAGIC);
    }

    #[test]
    fn empty_profile_round_trips() {
        let profile = Profile::default();
        let bytes = PprofFormat
            .write(&profile, WriteOptions::default())
            .unwrap();
        let parsed = PprofFormat.read(&bytes).unwrap();
        assert_eq!(parsed, profile);
    }

    #[test]
    fn reads_uncompressed_protobuf() {
        // A reader must accept a raw (non-gzipped) protobuf payload too.
        let proto = convert_to_proto(&sample_profile());
        let raw = proto.encode_to_vec();
        let parsed = PprofFormat.read(&raw).unwrap();
        assert_eq!(parsed, sample_profile());
    }

    #[test]
    fn probe_accepts_own_output() {
        let bytes = PprofFormat
            .write(&sample_profile(), WriteOptions::default())
            .unwrap();
        let data = ProbeData {
            filename: None,
            buf: &bytes,
        };
        assert_eq!(PprofFormat.probe(&data), Confidence::Likely);
    }

    #[test]
    fn probe_rejects_garbage() {
        let data = ProbeData {
            filename: None,
            buf: b"not a profile, just some text bytes",
        };
        assert_eq!(PprofFormat.probe(&data), Confidence::None);
    }

    #[test]
    fn rejects_value_arity_mismatch() {
        // Two declared value kinds but a sample carrying one value.
        let mut proto = convert_to_proto(&sample_profile());
        proto.sample[0].value = vec![1];
        let raw = proto.encode_to_vec();
        let error = PprofFormat.read(&raw).unwrap_err();
        assert!(matches!(error, ProfcastError::Decode(_)));
    }

    #[test]
    fn rejects_out_of_range_string_index() {
        let mut proto = convert_to_proto(&sample_profile());
        proto.sample_type[0].r#type = 9999;
        let raw = proto.encode_to_vec();
        let error = PprofFormat.read(&raw).unwrap_err();
        assert!(matches!(error, ProfcastError::Decode(_)));
    }

    #[test]
    fn reads_protoc_encoded_fixture() {
        // Encoded by `protoc` (an independent encoder), not our own writer, so
        // this exercises real interop. See testdata/sample.pb.gz.
        let bytes = include_bytes!("testdata/sample.pb.gz");
        let profile = PprofFormat.read(bytes).unwrap();
        profile.validate().unwrap();

        assert_eq!(
            profile.value_kinds,
            vec![
                ValueKind {
                    kind: "samples".to_owned(),
                    unit: "count".to_owned()
                },
                ValueKind {
                    kind: "cpu".to_owned(),
                    unit: "nanoseconds".to_owned()
                },
            ],
        );

        // Frames are interned in location order: main (id 1) then work (id 2).
        assert_eq!(profile.frames.len(), 2);
        let main = &profile.frames[0];
        assert_eq!(main.function.as_deref(), Some("main"));
        assert_eq!(main.file.as_deref(), Some("main.go"));
        assert_eq!(main.line, Some(10));
        assert_eq!(main.module.as_deref(), Some("/app/bin"));
        let work = &profile.frames[1];
        assert_eq!(work.function.as_deref(), Some("work"));
        assert_eq!(work.line, Some(20));

        // Sample 0 is main -> work (root-first) with both value series.
        assert_eq!(profile.samples.len(), 2);
        assert_eq!(profile.samples[0].stack, vec![FrameId(0), FrameId(1)]);
        assert_eq!(profile.samples[0].values, vec![5, 500]);
        assert_eq!(profile.samples[1].stack, vec![FrameId(0)]);
        assert_eq!(profile.samples[1].values, vec![3, 300]);
    }

    #[test]
    fn expands_inlined_lines_into_frames() {
        // A single location with two lines (memcpy inlined into printf) must
        // produce two frames, innermost first after the leaf-to-root reversal.
        let mut strings = StringTable::new();
        let memcpy = strings.intern("memcpy");
        let printf = strings.intern("printf");
        let proto = proto::Profile {
            sample_type: vec![proto::ValueType {
                r#type: strings.intern("samples"),
                unit: strings.intern("count"),
            }],
            sample: vec![proto::Sample {
                location_id: vec![1],
                value: vec![3],
                ..Default::default()
            }],
            function: vec![
                proto::Function {
                    id: 1,
                    name: memcpy,
                    ..Default::default()
                },
                proto::Function {
                    id: 2,
                    name: printf,
                    ..Default::default()
                },
            ],
            location: vec![proto::Location {
                id: 1,
                line: vec![
                    proto::Line {
                        function_id: 1,
                        ..Default::default()
                    },
                    proto::Line {
                        function_id: 2,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            string_table: strings.into_vec(),
            ..Default::default()
        };
        let parsed = convert_from_proto(&proto).unwrap();
        let labels: Vec<_> = parsed.samples[0]
            .stack
            .iter()
            .map(|id| parsed.frames[id.0 as usize].function.as_deref())
            .collect();
        // Stack is root-first: caller (printf) then leaf (memcpy).
        assert_eq!(labels, vec![Some("printf"), Some("memcpy")]);
    }
}
