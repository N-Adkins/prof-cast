//! The ".folded" profiler format
//!
//! Folded (a.k.a. "collapsed") stacks are the textual format popularised by
//! Brendan Gregg's `FlameGraph` tooling. Each non-empty, non-comment line is a
//! single aggregated stack of the shape:
//!
//! ```text
//! root_frame;middle_frame;leaf_frame <count>
//! ```
//!
//! Frames are separated by `;` and ordered outermost (root) to innermost
//! (leaf). The weight is the final whitespace-separated token and is a
//! non-negative integer sample count. Lines that are empty or begin with `#`
//! are treated as comments and ignored.

use std::collections::HashMap;
use std::path::Path;

use profcast_core::{
    Result,
    error::ProfcastError,
    format::{Confidence, InputFormat, OutputFormat, ProbeData, WriteOptions},
    model::{Frame, FrameId, Profile, Sample, ValueKind},
};

/// Number of leading lines the prober inspects before making up its mind.
const PROBE_LINE_BUDGET: usize = 32;

/// Internal state for the folded format.
///
/// The format is stateless, so this is a zero-sized marker that exists only to
/// implement [`InputFormat`].
#[derive(Debug, Default, Clone, Copy)]
pub struct FoldedFormat;

/// One parsed folded line: the stack portion and its weight.
struct FoldedLine<'a> {
    /// The `;`-separated stack, outermost first. Never empty.
    stack: &'a str,
    /// The non-negative sample count.
    count: i64,
}

/// Classification of a single physical line of input.
enum LineKind<'a> {
    /// Blank line or `#` comment - carries no data and is skipped.
    Skip,
    /// A well-formed folded stack line.
    Stack(FoldedLine<'a>),
    /// A line that is not valid folded grammar, with a reason.
    Invalid(&'static str),
}

/// Classifies a single line against the folded grammar.
///
/// This is the single source of truth shared by [`FoldedFormat::probe`] and
/// [`FoldedFormat::read`] so that detection and parsing can never disagree.
fn classify_line(line: &str) -> LineKind<'_> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return LineKind::Skip;
    }

    // The weight is the final whitespace-delimited token; everything before it
    // is the stack. Splitting from the right keeps frames that contain spaces
    // (e.g. `Foo::bar(int, int)`) intact.
    let Some((stack, count)) = line.rsplit_once(char::is_whitespace) else {
        // A lone non-negative integer is a frameless sample (py-spy emits these);
        // accept it as an empty, skipped stack. Any other lone token is malformed.
        return match line.parse::<i64>() {
            Ok(count) if count >= 0 => LineKind::Skip,
            _ => LineKind::Invalid("missing whitespace-separated sample count"),
        };
    };

    let stack = stack.trim_end();

    let Ok(count) = count.trim().parse::<i64>() else {
        return LineKind::Invalid("sample count is not an integer");
    };
    if count < 0 {
        return LineKind::Invalid("sample count is negative");
    }

    // A stack of only whitespace or separators (e.g. `;; 1`) is frameless too.
    if stack.is_empty() {
        return LineKind::Skip;
    }

    LineKind::Stack(FoldedLine { stack, count })
}

/// Inspects leading bytes and reports how strongly they resemble folded data.
fn probe_content(buf: &[u8]) -> Confidence {
    if buf.is_empty() {
        return Confidence::None;
    }

    // Folded is a text format. Decode lossily so a multi-byte character cut off
    // at the end of a header slice doesn't sink an otherwise valid probe.
    let text = String::from_utf8_lossy(buf);

    // If the buffer doesn't end in a newline it was likely truncated mid-line,
    // so drop the final (partial) line rather than judge it.
    let ends_clean = text.ends_with('\n');
    let mut lines = text.lines().peekable();

    let mut valid = 0_usize;
    let mut seen = 0_usize;
    while let Some(line) = lines.next() {
        if seen >= PROBE_LINE_BUDGET {
            break;
        }
        // Skip the trailing partial line of a truncated buffer.
        if !ends_clean && lines.peek().is_none() {
            break;
        }
        match classify_line(line) {
            LineKind::Skip => {}
            LineKind::Stack(_) => {
                valid = valid.saturating_add(1);
                seen = seen.saturating_add(1);
            }
            // A single grammar violation means this isn't folded; the format is
            // line-oriented and homogeneous.
            LineKind::Invalid(_) => return Confidence::None,
        }
    }

    if valid > 0 {
        // Leading lines obey the grammar.
        Confidence::Likely
    } else {
        // Valid-ish text but nothing we could confirm (e.g. all comments).
        Confidence::Weak
    }
}

/// Parses a bare hexadecimal address such as `0x7f4060475e44`.
///
/// Requires the whole token to be a `0x`-prefixed hex literal so that ordinary
/// symbol names are never mistaken for addresses.
fn parse_hex_address(token: &str) -> Option<u64> {
    let hex = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))?;
    if hex.is_empty() {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

/// Splits a `file:line` annotation into its file and parsed line number.
///
/// Returns `None` when the trailing `:`-segment is not an integer line, which is
/// how a module annotation (e.g. `libc.so.6`) is told apart from a source
/// location (e.g. `base64.py:304`).
fn split_file_line(annotation: &str) -> Option<(&str, u32)> {
    let (file, line) = annotation.rsplit_once(':')?;
    if file.is_empty() {
        return None;
    }
    Some((file, line.parse::<u32>().ok()?))
}

/// Best-effort extraction of structured fields from a folded frame label.
///
/// Folded stacks have no formal sub-grammar for a frame; producers annotate the
/// bare symbol differently. This recognises the following shapes emitted by perf's
/// `stackcollapse` and `py-spy` when I tried them:
///
/// - `symbol (file.ext:line)` -> function + file + line
/// - `symbol (module)`        -> function + module (e.g. `libc.so.6`)
/// - `0xADDR (module)`        -> address + module
/// - `symbol`                 -> function only
///
/// TODO flesh this out.
///
/// The full original label is always preserved in [`Frame::raw`], and anything
/// that does not match falls back to treating the whole label as the function
/// name, so no information is ever lost.
fn parse_frame(label: &str) -> Frame {
    let mut frame = Frame {
        raw: label.to_owned(),
        ..Frame::default()
    };

    // Peel off a trailing " (...)" annotation. The leading space distinguishes a
    // real annotation from parentheses that belong to a signature such as
    // `Foo::bar(int, int)`.
    let (symbol, annotation) = label
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" ("))
        .map_or_else(
            || (label.trim(), None),
            |(symbol, annotation)| (symbol.trim(), Some(annotation)),
        );

    // The symbol is either a raw address or a function name.
    if let Some(address) = parse_hex_address(symbol) {
        frame.address = Some(address);
    } else if !symbol.is_empty() {
        frame.function = Some(symbol.to_owned());
    }

    if let Some(annotation) = annotation {
        if let Some((file, line)) = split_file_line(annotation) {
            frame.file = Some(file.to_owned());
            frame.line = Some(line);
        } else {
            frame.module = Some(annotation.to_owned());
        }
    }

    frame
}

/// Builds a [`Profile`]'s interned frame table while parsing.
#[derive(Default)]
struct FrameInterner {
    frames: Vec<Frame>,
    index: HashMap<String, FrameId>,
}

impl FrameInterner {
    /// Returns the stable id for `label`, interning it on first sight.
    fn intern(&mut self, label: &str) -> FrameId {
        if let Some(id) = self.index.get(label) {
            return *id;
        }
        // Frame ids are u32; 4 billion distinct frames is not a real input, so
        // saturate rather than introduce a fallible path through every line.
        let id = FrameId(u32::try_from(self.frames.len()).unwrap_or(u32::MAX));
        self.frames.push(parse_frame(label));
        self.index.insert(label.to_owned(), id);
        id
    }
}

impl InputFormat for FoldedFormat {
    fn name(&self) -> &'static str {
        "folded"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["folded", "collapsed"]
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
            "probed folded format",
        );
        confidence
    }

    #[tracing::instrument(
        level = "debug",
        name = "folded.read",
        skip_all,
        fields(bytes = input.len())
    )]
    fn read(&self, input: &[u8]) -> Result<Profile> {
        let text = std::str::from_utf8(input)?;

        let mut interner = FrameInterner::default();
        let mut samples = Vec::new();

        for (idx, raw_line) in text.lines().enumerate() {
            let line = match classify_line(raw_line) {
                LineKind::Skip => continue,
                LineKind::Stack(line) => line,
                LineKind::Invalid(reason) => {
                    tracing::debug!(
                        line = idx.saturating_add(1),
                        reason,
                        "rejecting folded input"
                    );
                    return Err(ProfcastError::Parse {
                        line: idx.saturating_add(1),
                        message: reason.to_owned(),
                    });
                }
            };

            // Drop empty segments produced by a stray leading/trailing `;`.
            let stack = line
                .stack
                .split(';')
                .map(str::trim)
                .filter(|frame| !frame.is_empty())
                .map(|frame| interner.intern(frame))
                .collect::<Vec<_>>();

            if stack.is_empty() {
                // Frameless sample (e.g. a line of only `;`); skip it.
                continue;
            }

            samples.push(Sample {
                stack,
                values: vec![line.count],
            });
        }

        tracing::debug!(
            samples = samples.len(),
            frames = interner.frames.len(),
            "parsed folded profile",
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

/// Renders a [`Frame`] back to a folded stack label.
///
/// This is the inverse of [`parse_frame`]. If it works properly, then
/// folded -> model -> folded should result in the same input and output data.
fn render_frame(frame: &Frame) -> String {
    if !frame.raw.is_empty() {
        return frame.raw.clone();
    }

    // Symbol: prefer the human-readable function name (pprof frames can carry
    // both); fall back to a bare address.
    let symbol = frame
        .function
        .clone()
        .or_else(|| frame.address.map(|address| format!("0x{address:x}")))
        .unwrap_or_default();

    // Annotation: a `(file:line)` source location or a `(module)` tag.
    match (&frame.file, frame.line, &frame.module) {
        (Some(file), Some(line), _) => format!("{symbol} ({file}:{line})"),
        (Some(file), None, _) => format!("{symbol} ({file})"),
        (None, _, Some(module)) => format!("{symbol} ({module})"),
        (None, _, None) => symbol,
    }
}

impl OutputFormat for FoldedFormat {
    fn name(&self) -> &'static str {
        "folded"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["folded", "collapsed"]
    }

    #[tracing::instrument(
        level = "debug",
        name = "folded.write",
        skip_all,
        fields(samples = profile.samples.len())
    )]
    fn write(&self, profile: &Profile, _options: WriteOptions) -> Result<Vec<u8>> {
        let frame_count = profile.frames.len();
        let mut out = String::new();

        for (index, sample) in profile.samples.iter().enumerate() {
            // Folded carries a single weight; the model may hold several value
            // series, so we emit the first and drop the rest (folded is lossy).
            let Some(&count) = sample.values.first() else {
                let message = format!("sample {index} has no value to emit as a folded count");
                return Err(ProfcastError::InvalidProfile(message));
            };

            // Stack is outermost (root) first, frames joined by `;`.
            for (position, frame_id) in sample.stack.iter().enumerate() {
                let Some(frame) = profile.frames.get(frame_id.0 as usize) else {
                    let message = format!(
                        "sample {index} references frame id {} but only {frame_count} frames are interned",
                        frame_id.0,
                    );
                    return Err(ProfcastError::InvalidProfile(message));
                };
                if position > 0 {
                    out.push(';');
                }
                out.push_str(&render_frame(frame));
            }

            out.push(' ');
            out.push_str(&count.to_string());
            out.push('\n');
        }

        tracing::debug!(bytes = out.len(), "wrote folded profile");
        Ok(out.into_bytes())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn profile_of(input: &str) -> Profile {
        FoldedFormat.read(input.as_bytes()).unwrap()
    }

    #[test]
    fn parses_basic_stacks() {
        let profile = profile_of("a;b;c 10\na;b 5\n");

        assert_eq!(profile.samples.len(), 2);
        assert_eq!(profile.value_kinds.len(), 1);
        assert_eq!(profile.value_kinds[0].kind, "samples");

        // `a`, `b`, `c` interned once each, in first-seen order.
        let labels: Vec<_> = profile.frames.iter().map(|f| f.raw.as_str()).collect();
        assert_eq!(labels, ["a", "b", "c"]);

        assert_eq!(
            profile.samples[0].stack,
            vec![FrameId(0), FrameId(1), FrameId(2)]
        );
        assert_eq!(profile.samples[0].values, vec![10]);
        assert_eq!(profile.samples[1].stack, vec![FrameId(0), FrameId(1)]);
        assert_eq!(profile.samples[1].values, vec![5]);
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let profile = profile_of("# a comment\n\nmain;work 3\n   \n");
        assert_eq!(profile.samples.len(), 1);
        assert_eq!(profile.samples[0].values, vec![3]);
    }

    #[test]
    fn keeps_frames_containing_spaces() {
        let profile = profile_of("Foo::bar(int, int);baz 7\n");
        let labels: Vec<_> = profile.frames.iter().map(|f| f.raw.as_str()).collect();
        assert_eq!(labels, ["Foo::bar(int, int)", "baz"]);
        assert_eq!(profile.samples[0].values, vec![7]);
    }

    #[test]
    fn tolerates_stray_semicolons() {
        let profile = profile_of("a;;b; 4\n");
        let labels: Vec<_> = profile.frames.iter().map(|f| f.raw.as_str()).collect();
        assert_eq!(labels, ["a", "b"]);
    }

    #[test]
    fn skips_frameless_samples() {
        // py-spy emits a bare count for a sample with no frame; a `;`-only stack
        // is frameless too. Both are skipped, not rejected.
        let profile = profile_of("main;work 3\n1\n;; 2\n");
        assert_eq!(profile.samples.len(), 1);
        assert_eq!(profile.samples[0].values, vec![3]);
    }

    #[test]
    fn bare_numbers_are_not_likely_folded() {
        // A frameless line is no positive evidence, so a column of numbers must
        // not auto-detect as folded.
        let data = ProbeData {
            filename: None,
            buf: b"1\n2\n3\n",
        };
        assert_ne!(FoldedFormat.probe(&data), Confidence::Likely);
    }

    #[test]
    fn rejects_missing_count() {
        let err = FoldedFormat.read(b"a;b;c\n").unwrap_err();
        assert!(matches!(err, ProfcastError::Parse { line: 1, .. }));
    }

    #[test]
    fn rejects_non_integer_count() {
        let err = FoldedFormat.read(b"a;b 1\na;c xx\n").unwrap_err();
        assert!(matches!(err, ProfcastError::Parse { line: 2, .. }));
    }

    #[test]
    fn rejects_negative_count() {
        let err = FoldedFormat.read(b"a;b -3\n").unwrap_err();
        assert!(matches!(err, ProfcastError::Parse { line: 1, .. }));
    }

    #[test]
    fn rejects_invalid_utf8() {
        let err = FoldedFormat.read(&[0xff, 0xfe]).unwrap_err();
        assert!(matches!(err, ProfcastError::Utf8(_)));
    }

    #[test]
    fn probe_recognizes_grammar() {
        let data = ProbeData {
            filename: None,
            buf: b"a;b;c 10\nd;e 2\n",
        };
        assert_eq!(FoldedFormat.probe(&data), Confidence::Likely);
    }

    #[test]
    fn probe_extension_only_for_garbage_text() {
        let data = ProbeData {
            filename: Some("profile.folded"),
            buf: b"this is not folded at all\n",
        };
        // Content is rejected, but the extension still lends weak confidence.
        assert_eq!(FoldedFormat.probe(&data), Confidence::Extension);
    }

    #[test]
    fn probe_ignores_truncated_trailing_line() {
        // No trailing newline: the final, possibly-cut line must not be judged.
        let data = ProbeData {
            filename: None,
            buf: b"a;b;c 10\nd;e;f;g;par",
        };
        assert_eq!(FoldedFormat.probe(&data), Confidence::Likely);
    }

    #[test]
    fn probe_empty_is_none() {
        let data = ProbeData {
            filename: None,
            buf: b"",
        };
        assert_eq!(FoldedFormat.probe(&data), Confidence::None);
    }

    #[test]
    fn parses_python_frame_with_file_and_line() {
        let frame = parse_frame("_85encode (base64.py:304)");
        assert_eq!(frame.raw, "_85encode (base64.py:304)");
        assert_eq!(frame.function.as_deref(), Some("_85encode"));
        assert_eq!(frame.file.as_deref(), Some("base64.py"));
        assert_eq!(frame.line, Some(304));
        assert_eq!(frame.module, None);
        assert_eq!(frame.address, None);
    }

    #[test]
    fn parses_native_frame_with_module() {
        let frame = parse_frame("BZ2_blockSort (libbz2.so.1.0.8)");
        assert_eq!(frame.function.as_deref(), Some("BZ2_blockSort"));
        assert_eq!(frame.module.as_deref(), Some("libbz2.so.1.0.8"));
        assert_eq!(frame.file, None);
        assert_eq!(frame.line, None);
        assert_eq!(frame.address, None);
    }

    #[test]
    fn parses_hex_address_with_module() {
        let frame = parse_frame("0x7f4060475e44 (libsqlite3.so.3.51.2)");
        assert_eq!(frame.address, Some(0x7f40_6047_5e44));
        assert_eq!(frame.module.as_deref(), Some("libsqlite3.so.3.51.2"));
        assert_eq!(frame.function, None);
        assert_eq!(frame.file, None);
        assert_eq!(frame.line, None);
    }

    #[test]
    fn keeps_signature_parens_as_function() {
        let frame = parse_frame("Foo::bar(int, int)");
        assert_eq!(frame.function.as_deref(), Some("Foo::bar(int, int)"));
        assert_eq!(frame.file, None);
        assert_eq!(frame.module, None);
        assert_eq!(frame.address, None);
    }

    #[test]
    fn splits_module_after_a_signature() {
        let frame = parse_frame("Foo::bar(int, int) (/usr/lib/foo.so)");
        assert_eq!(frame.function.as_deref(), Some("Foo::bar(int, int)"));
        assert_eq!(frame.module.as_deref(), Some("/usr/lib/foo.so"));
        assert_eq!(frame.file, None);
    }

    #[test]
    fn splits_file_path_with_line() {
        let frame = parse_frame("walk (/usr/lib/python3.14/ast.py:397)");
        assert_eq!(frame.function.as_deref(), Some("walk"));
        assert_eq!(frame.file.as_deref(), Some("/usr/lib/python3.14/ast.py"));
        assert_eq!(frame.line, Some(397));
    }

    #[test]
    fn bare_symbol_is_function_only() {
        let frame = parse_frame("main");
        assert_eq!(frame.function.as_deref(), Some("main"));
        assert_eq!(frame.file, None);
        assert_eq!(frame.line, None);
        assert_eq!(frame.module, None);
        assert_eq!(frame.address, None);
    }

    #[test]
    fn read_populates_structured_frame_fields() {
        let profile = profile_of("main (app.py:7);brk (libc.so.6) 4\n");
        let fields: Vec<_> = profile
            .frames
            .iter()
            .map(|frame| {
                (
                    frame.function.as_deref(),
                    frame.file.as_deref(),
                    frame.line,
                    frame.module.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            fields,
            vec![
                (Some("main"), Some("app.py"), Some(7), None),
                (Some("brk"), None, None, Some("libc.so.6")),
            ],
        );
    }

    fn write_string(profile: &Profile) -> String {
        let bytes = FoldedFormat
            .write(profile, WriteOptions::default())
            .unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn write_round_trips_through_folded() {
        // Frames keep their `raw` label, so a parse then a write reproduces the
        // input byte for byte (frames are emitted root -> leaf, weight last).
        let input = "a;b;c 10\na;b 5\n";
        assert_eq!(write_string(&profile_of(input)), input);
    }

    #[test]
    fn write_round_trips_annotated_frames() {
        let input = "main (app.py:7);brk (libc.so.6) 4\n";
        assert_eq!(write_string(&profile_of(input)), input);
    }

    #[test]
    fn write_reconstructs_labels_without_raw() {
        // Frames sourced from another format carry no `raw`; the label is
        // rebuilt from the structured fields, mirroring `parse_frame`.
        let profile = Profile {
            frames: vec![
                Frame {
                    function: Some("main".to_owned()),
                    file: Some("main.rs".to_owned()),
                    line: Some(42),
                    ..Frame::default()
                },
                Frame {
                    address: Some(0x7f40_6047_5e44),
                    module: Some("libc.so.6".to_owned()),
                    ..Frame::default()
                },
            ],
            samples: vec![Sample {
                stack: vec![FrameId(0), FrameId(1)],
                values: vec![3],
            }],
            value_kinds: vec![ValueKind {
                kind: "samples".to_owned(),
                unit: "count".to_owned(),
            }],
        };
        assert_eq!(
            write_string(&profile),
            "main (main.rs:42);0x7f4060475e44 (libc.so.6) 3\n",
        );
    }

    #[test]
    fn write_prefers_function_over_address() {
        // pprof frames carry both a function name and an instruction address;
        // folded should render the symbol, not the hex address.
        let profile = Profile {
            frames: vec![Frame {
                function: Some("main.serve".to_owned()),
                file: Some("server.go".to_owned()),
                line: Some(51),
                address: Some(0x4c_6fea),
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
        assert_eq!(write_string(&profile), "main.serve (server.go:51) 1\n");
    }

    #[test]
    fn write_uses_only_the_first_value_series() {
        let profile = Profile {
            frames: vec![Frame {
                raw: "a".to_owned(),
                ..Frame::default()
            }],
            samples: vec![Sample {
                stack: vec![FrameId(0)],
                values: vec![9, 99],
            }],
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
        };
        assert_eq!(write_string(&profile), "a 9\n");
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
        let err = FoldedFormat
            .write(&profile, WriteOptions::default())
            .unwrap_err();
        assert!(matches!(err, ProfcastError::InvalidProfile(_)));
    }

    #[test]
    fn write_rejects_sample_without_value() {
        let profile = Profile {
            frames: vec![Frame {
                raw: "a".to_owned(),
                ..Frame::default()
            }],
            samples: vec![Sample {
                stack: vec![FrameId(0)],
                values: vec![],
            }],
            value_kinds: vec![],
        };
        let err = FoldedFormat
            .write(&profile, WriteOptions::default())
            .unwrap_err();
        assert!(matches!(err, ProfcastError::InvalidProfile(_)));
    }
}
