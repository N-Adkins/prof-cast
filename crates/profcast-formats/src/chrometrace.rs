//! The Chrome Trace Event Format (`chrome://tracing`, Perfetto, catapult).
//!
//! This is a bidirectional JSON format. profcast reads and writes the *sampling*
//! subset of the spec: the top-level `stackFrames` map plus `samples` array (and
//! `"ph": "P"` sample events inside `traceEvents`). The duration-event side of
//! the format (`B`/`E`/`X` events) describes a fundamentally different, interval
//! based model and is ignored on read and never produced on write.
//!
//! A `stackFrames` entry is a node in the call tree: it carries a `name`, an
//! optional `category`, and an optional `parent` id pointing one frame closer to
//! the root. A sample references its leaf frame by `sf`; the full stack is
//! recovered by walking `parent` links up to the root. On write we rebuild that
//! tree as a trie over every sample's stack, so shared prefixes are interned
//! exactly as a real tracer would emit them.
//!
//! Like folded, the format carries a single weight per sample, so writing keeps
//! only the first value series (see [`Profile::value_kinds`]). Chrome traces
//! conventionally use the `.json` extension, which is already claimed by
//! [`JsonFormat`](crate::json::JsonFormat); this format is therefore selected by
//! name (`--from chrometrace` / `--to chrometrace`) or auto-detected from its
//! content, never inferred from a path.
//!
//! See <https://docs.google.com/document/d/1CvAClvFfyA5R-PhYUmn5OOQtYMH4h6I0nSsKchNAySU>.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use profcast_core::{
    Result,
    error::ProfcastError,
    format::{Confidence, InputFormat, OutputFormat, ProbeData, WriteOptions},
    model::{Frame, FrameId, Profile, Sample, ValueKind},
};

/// The Chrome trace format. Stateless, so this is a zero-sized marker that
/// implements both [`InputFormat`] and [`OutputFormat`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ChromeTraceFormat;

/// The slice of a trace file profcast understands: the stack-frame tree, the
/// sample array, and any inline sample events. Unknown top-level keys
/// (`displayTimeUnit`, `metadata`, ...) are ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceObject {
    /// The call-tree nodes, keyed by string id.
    #[serde(default)]
    stack_frames: HashMap<String, RawStackFrame>,
    /// Top-level sampling records (object-format traces).
    #[serde(default)]
    samples: Vec<RawSample>,
    /// Inline events; only `"ph": "P"` sample events are read.
    #[serde(default)]
    trace_events: Vec<RawEvent>,
}

/// One node in the `stackFrames` tree.
#[derive(Debug, Deserialize)]
struct RawStackFrame {
    /// Human-readable frame label.
    name: String,
    /// Optional grouping (module / library); mapped to [`Frame::module`].
    #[serde(default)]
    category: Option<String>,
    /// The id of the parent frame, one step closer to the root.
    #[serde(default)]
    parent: Option<FrameRef>,
}

/// A sampling record from the top-level `samples` array.
#[derive(Debug, Deserialize)]
struct RawSample {
    /// Leaf stack-frame id; a sample without one carries no stack.
    #[serde(default)]
    sf: Option<FrameRef>,
    /// Sample weight; absent means a weight of one.
    #[serde(default)]
    weight: Option<i64>,
}

/// A `traceEvents` entry, of which only `"ph": "P"` samples are relevant.
#[derive(Debug, Deserialize)]
struct RawEvent {
    /// Event phase. `"P"` marks a sample event.
    #[serde(default)]
    ph: Option<String>,
    /// Leaf stack-frame id for a sample event.
    #[serde(default)]
    sf: Option<FrameRef>,
    /// Sample weight; absent means a weight of one.
    #[serde(default)]
    weight: Option<i64>,
}

/// A stack-frame id reference, which the spec allows to be either a string or a
/// number. Both are normalised to their decimal string form for lookup.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FrameRef {
    /// A string id, e.g. `"3"` or `"main:0x1234"`.
    Str(String),
    /// A numeric id, e.g. `3`.
    Int(i64),
}

impl FrameRef {
    /// The canonical map-key form of this reference.
    fn key(&self) -> String {
        match self {
            Self::Str(value) => value.clone(),
            Self::Int(value) => value.to_string(),
        }
    }
}

/// Builds the interned [`Frame`] table while resolving stacks.
///
/// Chrome stack frames are tree nodes, so the same function appears under many
/// ids; interning by `(name, category)` collapses those back into one model
/// frame, matching how the other readers dedupe.
#[derive(Default)]
struct FrameInterner {
    frames: Vec<Frame>,
    index: HashMap<(String, Option<String>), FrameId>,
}

impl FrameInterner {
    /// Returns the stable model id for a trace node, interning on first sight.
    fn intern(&mut self, node: &RawStackFrame) -> FrameId {
        let key = (node.name.clone(), node.category.clone());
        if let Some(id) = self.index.get(&key) {
            return *id;
        }
        // Frame ids are u32; saturate rather than thread a fallible path through
        // every node, matching the folded reader.
        let id = FrameId(u32::try_from(self.frames.len()).unwrap_or(u32::MAX));
        self.frames.push(Frame {
            raw: node.name.clone(),
            function: (!node.name.is_empty()).then(|| node.name.clone()),
            module: node.category.clone(),
            ..Frame::default()
        });
        self.index.insert(key, id);
        id
    }
}

/// Walks the `parent` chain from a leaf frame to the root, returning the stack
/// outermost-first (the order the model expects).
///
/// Errors if a referenced id is missing, or if the chain is longer than the
/// frame table (which can only happen on a malformed parent cycle).
fn resolve_stack(
    leaf: &str,
    nodes: &HashMap<String, RawStackFrame>,
    interner: &mut FrameInterner,
) -> Result<Vec<FrameId>> {
    let limit = nodes.len().saturating_add(1);
    let mut stack = Vec::new();
    let mut current = Some(leaf.to_owned());
    let mut steps = 0_usize;

    while let Some(id) = current {
        if steps > limit {
            return Err(ProfcastError::Decode(format!(
                "stack frame chain from '{leaf}' exceeds {limit} frames (parent cycle?)"
            )));
        }
        steps = steps.saturating_add(1);

        let Some(node) = nodes.get(&id) else {
            return Err(ProfcastError::Decode(format!(
                "sample references unknown stack frame id '{id}'"
            )));
        };
        stack.push(interner.intern(node));
        current = node.parent.as_ref().map(FrameRef::key);
    }

    // Collected leaf -> root; the model wants root -> leaf.
    stack.reverse();
    Ok(stack)
}

/// Pairs every sampling record (from `samples` and inline `"P"` events) with the
/// leaf frame id it references and its weight.
fn collect_records(trace: &TraceObject) -> Vec<(String, i64)> {
    let mut records = Vec::new();
    for sample in &trace.samples {
        if let Some(sf) = &sample.sf {
            records.push((sf.key(), sample.weight.unwrap_or(1)));
        }
    }
    for event in &trace.trace_events {
        if event.ph.as_deref() == Some("P") {
            if let Some(sf) = &event.sf {
                records.push((sf.key(), event.weight.unwrap_or(1)));
            }
        }
    }
    records
}

/// Decodes the top-level JSON into a [`TraceObject`], accepting both the object
/// form (`{ "traceEvents": [...] }`) and the bare-array form (`[ ... ]`).
fn decode_trace(input: &[u8]) -> Result<TraceObject> {
    let value: serde_json::Value = serde_json::from_slice(input)?;
    match value {
        serde_json::Value::Object(_) => Ok(serde_json::from_value(value)?),
        serde_json::Value::Array(_) => Ok(TraceObject {
            trace_events: serde_json::from_value(value)?,
            ..TraceObject::default()
        }),
        _ => Err(ProfcastError::Decode(
            "expected a JSON object or array at the top level".to_owned(),
        )),
    }
}

/// Picks the display name for a [`Frame`], preferring the symbol over the raw
/// label, then the module, then a hex address, so a frame is never anonymous.
fn frame_name(frame: &Frame) -> String {
    if let Some(function) = &frame.function {
        if !function.is_empty() {
            return function.clone();
        }
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

/// A node in the rebuilt stack-frame trie: a model frame plus the trie id of its
/// parent (the frame one step closer to the root).
struct TrieNode {
    /// Index into [`Profile::frames`].
    frame: u32,
    /// Trie id of the parent node, or `None` at the root.
    parent: Option<u32>,
}

/// A prefix tree over sample stacks. Each distinct (parent, frame) pair is one
/// node, so a leaf id uniquely identifies a whole root-to-leaf path - exactly
/// the `stackFrames`/`sf` encoding chrome expects.
#[derive(Default)]
struct StackTrie {
    nodes: Vec<TrieNode>,
    index: HashMap<(Option<u32>, u32), u32>,
}

impl StackTrie {
    /// Interns a full stack (root-first) and returns the id of its leaf node, or
    /// `None` for an empty stack.
    ///
    /// Errors if a [`FrameId`] points outside the frame table, mirroring the
    /// guard the other writers apply.
    fn intern_path(&mut self, stack: &[FrameId], frame_count: usize) -> Result<Option<u32>> {
        let mut parent: Option<u32> = None;
        for frame_id in stack {
            if frame_id.0 as usize >= frame_count {
                return Err(ProfcastError::InvalidProfile(format!(
                    "sample references frame id {} but only {frame_count} frames are interned",
                    frame_id.0,
                )));
            }
            let key = (parent, frame_id.0);
            let node = if let Some(&existing) = self.index.get(&key) {
                existing
            } else {
                let id = u32::try_from(self.nodes.len()).unwrap_or(u32::MAX);
                self.nodes.push(TrieNode {
                    frame: frame_id.0,
                    parent,
                });
                self.index.insert(key, id);
                id
            };
            parent = Some(node);
        }
        Ok(parent)
    }
}

/// Top-level object profcast emits.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceOut {
    /// Always empty: profcast emits sampling data, not duration events.
    trace_events: [u8; 0],
    /// The interned stack-frame tree, keyed by decimal string id.
    stack_frames: BTreeMap<String, StackFrameOut>,
    /// One record per sample, referencing its leaf frame by `sf`.
    samples: Vec<SampleOut>,
    /// Hint for viewers; our timestamps are nominal nanoseconds.
    display_time_unit: &'static str,
}

/// One `stackFrames` entry.
#[derive(Serialize)]
struct StackFrameOut {
    /// Frame display name.
    name: String,
    /// Optional module / library grouping.
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    /// Parent frame id, omitted at the root.
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
}

/// One `samples` entry.
#[derive(Serialize)]
struct SampleOut {
    /// Thread id; the model is thread-agnostic, so this is fixed.
    tid: u32,
    /// Nominal timestamp, made monotonic so viewers order samples stably.
    ts: i64,
    /// Leaf frame display name, surfaced by some viewers.
    name: String,
    /// Leaf stack-frame id into `stackFrames`.
    sf: String,
    /// Sample weight, omitted when the profile carries no value series.
    #[serde(skip_serializing_if = "Option::is_none")]
    weight: Option<i64>,
}

/// Inspects leading bytes and reports how strongly they resemble a chrome trace.
fn probe_content(buf: &[u8]) -> Confidence {
    if buf.is_empty() {
        return Confidence::None;
    }
    // Cheap, slice-tolerant signal: the structural keys that mark a trace. A
    // `stackFrames` map means we can actually extract a sampled profile; a bare
    // `traceEvents` array is probably a duration trace we'd read as empty.
    let text = String::from_utf8_lossy(buf);
    if text.contains("\"stackFrames\"") {
        Confidence::Likely
    } else if text.contains("\"traceEvents\"") {
        Confidence::Weak
    } else {
        Confidence::None
    }
}

impl InputFormat for ChromeTraceFormat {
    fn name(&self) -> &'static str {
        "chrometrace"
    }

    fn probe(&self, data: &ProbeData<'_>) -> Confidence {
        let confidence = probe_content(data.buf);
        tracing::trace!(
            filename = ?data.filename,
            bytes = data.buf.len(),
            ?confidence,
            "probed chrome trace format",
        );
        confidence
    }

    #[tracing::instrument(
        level = "debug",
        name = "chrometrace.read",
        skip_all,
        fields(bytes = input.len())
    )]
    fn read(&self, input: &[u8]) -> Result<Profile> {
        let trace = decode_trace(input)?;
        let records = collect_records(&trace);

        let mut interner = FrameInterner::default();
        let mut samples = Vec::with_capacity(records.len());
        for (leaf, weight) in records {
            let stack = resolve_stack(&leaf, &trace.stack_frames, &mut interner)?;
            if stack.is_empty() {
                continue;
            }
            samples.push(Sample {
                stack,
                values: vec![weight],
            });
        }

        if samples.is_empty() {
            tracing::warn!(
                "no sampling data found; this trace may hold only duration events, which are unsupported",
            );
        }
        tracing::debug!(
            samples = samples.len(),
            frames = interner.frames.len(),
            "parsed chrome trace profile",
        );

        Ok(Profile {
            frames: interner.frames,
            samples,
            value_kinds: vec![ValueKind {
                kind: "samples".to_owned(),
                unit: "count".to_owned(),
            }],
        })
    }
}

impl OutputFormat for ChromeTraceFormat {
    fn name(&self) -> &'static str {
        "chrometrace"
    }

    #[tracing::instrument(
        level = "debug",
        name = "chrometrace.write",
        skip_all,
        fields(samples = profile.samples.len())
    )]
    fn write(&self, profile: &Profile, options: WriteOptions) -> Result<Vec<u8>> {
        let frame_count = profile.frames.len();
        let mut trie = StackTrie::default();
        let mut samples = Vec::with_capacity(profile.samples.len());
        let mut ts = 0_i64;

        for sample in &profile.samples {
            let Some(leaf) = trie.intern_path(&sample.stack, frame_count)? else {
                // A frameless sample has no stack to point `sf` at; drop it.
                continue;
            };
            // The leaf frame named the sample; it was range-checked above.
            let name = sample
                .stack
                .last()
                .and_then(|frame_id| profile.frames.get(frame_id.0 as usize))
                .map(frame_name)
                .unwrap_or_default();
            samples.push(SampleOut {
                tid: 0,
                ts,
                name,
                sf: leaf.to_string(),
                // Chrome carries one weight; emit the first series (lossy, as folded is).
                weight: sample.values.first().copied(),
            });
            ts = ts.saturating_add(1);
        }

        let mut stack_frames = BTreeMap::new();
        for (id, node) in trie.nodes.iter().enumerate() {
            let frame = profile.frames.get(node.frame as usize).ok_or_else(|| {
                ProfcastError::InvalidProfile(format!(
                    "stack node references frame id {} but only {frame_count} frames are interned",
                    node.frame,
                ))
            })?;
            stack_frames.insert(
                id.to_string(),
                StackFrameOut {
                    name: frame_name(frame),
                    category: frame.module.clone(),
                    parent: node.parent.map(|parent| parent.to_string()),
                },
            );
        }

        let out = TraceOut {
            trace_events: [],
            stack_frames,
            samples,
            display_time_unit: "ns",
        };

        let bytes = if options.pretty {
            serde_json::to_vec_pretty(&out)?
        } else {
            serde_json::to_vec(&out)?
        };
        tracing::debug!(
            bytes = bytes.len(),
            frames = out.stack_frames.len(),
            samples = out.samples.len(),
            "wrote chrome trace profile",
        );
        Ok(bytes)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use serde_json::Value;

    use super::*;

    /// Resolves a sample's stack to the display names of its frames, root-first.
    fn stack_names(profile: &Profile, sample: usize) -> Vec<String> {
        profile.samples[sample]
            .stack
            .iter()
            .map(|frame_id| frame_name(&profile.frames[frame_id.0 as usize]))
            .collect()
    }

    const OBJECT_TRACE: &str = r#"{
        "traceEvents": [],
        "displayTimeUnit": "ns",
        "stackFrames": {
            "1": {"name": "main", "category": "app"},
            "2": {"name": "work", "parent": "1"},
            "3": {"name": "leaf", "parent": "2"}
        },
        "samples": [
            {"tid": 0, "ts": 0, "sf": "3", "weight": 5},
            {"tid": 0, "ts": 1, "sf": "2"}
        ]
    }"#;

    #[test]
    fn reads_object_format_and_orders_stacks_root_first() {
        let profile = ChromeTraceFormat.read(OBJECT_TRACE.as_bytes()).unwrap();
        assert_eq!(profile.samples.len(), 2);
        assert_eq!(stack_names(&profile, 0), ["main", "work", "leaf"]);
        assert_eq!(profile.samples[0].values, vec![5]);
        // The second sample's leaf is `work`, so its stack stops there.
        assert_eq!(stack_names(&profile, 1), ["main", "work"]);
    }

    #[test]
    fn missing_weight_defaults_to_one() {
        let profile = ChromeTraceFormat.read(OBJECT_TRACE.as_bytes()).unwrap();
        assert_eq!(profile.samples[1].values, vec![1]);
    }

    #[test]
    fn interns_frames_by_name_and_category() {
        let profile = ChromeTraceFormat.read(OBJECT_TRACE.as_bytes()).unwrap();
        // main, work, leaf - each distinct, none duplicated despite many ids.
        assert_eq!(profile.frames.len(), 3);
        let main = profile
            .frames
            .iter()
            .find(|frame| frame.raw == "main")
            .unwrap();
        assert_eq!(main.module.as_deref(), Some("app"));
        assert_eq!(main.function.as_deref(), Some("main"));
    }

    #[test]
    fn reads_inline_p_sample_events() {
        let trace = r#"{
            "stackFrames": {"7": {"name": "f"}},
            "traceEvents": [
                {"ph": "P", "sf": "7", "weight": 3},
                {"ph": "X", "name": "ignored", "dur": 10}
            ]
        }"#;
        let profile = ChromeTraceFormat.read(trace.as_bytes()).unwrap();
        assert_eq!(profile.samples.len(), 1);
        assert_eq!(stack_names(&profile, 0), ["f"]);
        assert_eq!(profile.samples[0].values, vec![3]);
    }

    #[test]
    fn accepts_numeric_frame_ids() {
        // `sf`/`parent` given as numbers rather than strings.
        let trace = r#"{
            "stackFrames": {"1": {"name": "root"}, "2": {"name": "child", "parent": 1}},
            "samples": [{"sf": 2, "weight": 4}]
        }"#;
        let profile = ChromeTraceFormat.read(trace.as_bytes()).unwrap();
        assert_eq!(stack_names(&profile, 0), ["root", "child"]);
    }

    #[test]
    fn rejects_unknown_stack_frame_id() {
        let trace = r#"{"stackFrames": {}, "samples": [{"sf": "9"}]}"#;
        let error = ChromeTraceFormat.read(trace.as_bytes()).unwrap_err();
        assert!(matches!(error, ProfcastError::Decode(_)));
    }

    #[test]
    fn rejects_parent_cycle() {
        let trace = r#"{
            "stackFrames": {"1": {"name": "a", "parent": "2"}, "2": {"name": "b", "parent": "1"}},
            "samples": [{"sf": "1"}]
        }"#;
        let error = ChromeTraceFormat.read(trace.as_bytes()).unwrap_err();
        assert!(matches!(error, ProfcastError::Decode(_)));
    }

    #[test]
    fn empty_trace_reads_as_empty_profile() {
        let profile = ChromeTraceFormat.read(b"{}").unwrap();
        assert!(profile.samples.is_empty());
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn rejects_non_object_non_array_top_level() {
        let error = ChromeTraceFormat.read(b"42").unwrap_err();
        assert!(matches!(error, ProfcastError::Decode(_)));
    }

    fn sample_profile() -> Profile {
        Profile {
            frames: vec![
                Frame {
                    function: Some("main".to_owned()),
                    module: Some("app".to_owned()),
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
                    values: vec![10],
                },
                Sample {
                    stack: vec![FrameId(0)],
                    values: vec![5],
                },
            ],
            value_kinds: vec![ValueKind {
                kind: "samples".to_owned(),
                unit: "count".to_owned(),
            }],
        }
    }

    fn write_value(profile: &Profile) -> Value {
        let bytes = ChromeTraceFormat
            .write(profile, WriteOptions::default())
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn writes_shared_prefix_as_one_trie() {
        let value = write_value(&sample_profile());
        let frames = value["stackFrames"].as_object().unwrap();
        // `main` (shared root) + `work` = two nodes, not three.
        assert_eq!(frames.len(), 2);
        // The two samples reference the leaf and the shared root respectively.
        let samples = value["samples"].as_array().unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0]["weight"], 10);
        assert_eq!(samples[1]["weight"], 5);
    }

    #[test]
    fn write_links_children_to_parents() {
        let value = write_value(&sample_profile());
        let frames = value["stackFrames"].as_object().unwrap();
        // Find the `work` node and confirm its parent is the `main` node.
        let (work_id, work) = frames
            .iter()
            .find(|(_, frame)| frame["name"] == "work")
            .unwrap();
        let parent_id = work["parent"].as_str().unwrap();
        assert_eq!(frames[parent_id]["name"], "main");
        // The deeper sample points at the `work` node.
        let samples = value["samples"].as_array().unwrap();
        assert_eq!(samples[0]["sf"].as_str().unwrap(), work_id);
    }

    #[test]
    fn round_trips_through_chrome_trace() {
        let original = sample_profile();
        let bytes = ChromeTraceFormat
            .write(&original, WriteOptions::default())
            .unwrap();
        let reparsed = ChromeTraceFormat.read(&bytes).unwrap();

        assert_eq!(reparsed.samples.len(), original.samples.len());
        assert_eq!(stack_names(&reparsed, 0), ["main", "work"]);
        assert_eq!(reparsed.samples[0].values, vec![10]);
        assert_eq!(stack_names(&reparsed, 1), ["main"]);
        assert_eq!(reparsed.samples[1].values, vec![5]);
        // Category survives the round trip via Frame::module.
        let main = reparsed
            .frames
            .iter()
            .find(|frame| frame_name(frame) == "main")
            .unwrap();
        assert_eq!(main.module.as_deref(), Some("app"));
    }

    #[test]
    fn write_rejects_dangling_frame_id() {
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
        let error = ChromeTraceFormat
            .write(&profile, WriteOptions::default())
            .unwrap_err();
        assert!(matches!(error, ProfcastError::InvalidProfile(_)));
    }

    #[test]
    fn pretty_is_multiline_compact_is_not() {
        let profile = sample_profile();
        let pretty = ChromeTraceFormat
            .write(&profile, WriteOptions { pretty: true })
            .unwrap();
        let compact = ChromeTraceFormat
            .write(&profile, WriteOptions { pretty: false })
            .unwrap();
        assert!(pretty.contains(&b'\n'));
        assert!(!compact.contains(&b'\n'));
    }

    #[test]
    fn probe_recognizes_stack_frames() {
        let data = ProbeData {
            filename: None,
            buf: OBJECT_TRACE.as_bytes(),
        };
        assert_eq!(ChromeTraceFormat.probe(&data), Confidence::Likely);
    }

    #[test]
    fn probe_is_weak_for_duration_only_trace() {
        let data = ProbeData {
            filename: None,
            buf: br#"{"traceEvents": [{"ph": "X", "dur": 5}]}"#,
        };
        assert_eq!(ChromeTraceFormat.probe(&data), Confidence::Weak);
    }

    #[test]
    fn probe_rejects_unrelated_json() {
        let data = ProbeData {
            filename: None,
            buf: br#"{"hello": "world"}"#,
        };
        assert_eq!(ChromeTraceFormat.probe(&data), Confidence::None);
    }

    #[test]
    fn advertises_chrometrace_name() {
        assert_eq!(InputFormat::name(&ChromeTraceFormat), "chrometrace");
        assert_eq!(OutputFormat::name(&ChromeTraceFormat), "chrometrace");
    }
}
