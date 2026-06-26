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
    format::{Confidence, InputFormat, ProbeData},
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
        return LineKind::Invalid("missing whitespace-separated sample count");
    };

    let stack = stack.trim_end();
    if stack.is_empty() {
        return LineKind::Invalid("empty stack");
    }

    let Ok(count) = count.trim().parse::<i64>() else {
        return LineKind::Invalid("sample count is not an integer");
    };
    if count < 0 {
        return LineKind::Invalid("sample count is negative");
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
                valid += 1;
                seen += 1;
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
        self.frames.push(Frame {
            raw: label.to_owned(),
            ..Frame::default()
        });
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
            .is_some_and(|ext| self.extensions().iter().any(|expected| ext == *expected));
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

    fn read(&self, input: &[u8]) -> Result<Profile> {
        let span = tracing::debug_span!("folded.read", bytes = input.len());
        let _guard = span.enter();

        let text = std::str::from_utf8(input)?;

        let mut interner = FrameInterner::default();
        let mut samples = Vec::new();

        for (idx, raw_line) in text.lines().enumerate() {
            let line = match classify_line(raw_line) {
                LineKind::Skip => continue,
                LineKind::Stack(line) => line,
                LineKind::Invalid(reason) => {
                    tracing::debug!(line = idx + 1, reason, "rejecting folded input");
                    return Err(ProfcastError::Parse {
                        line: idx + 1,
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
                tracing::debug!(line = idx + 1, "rejecting folded input");
                return Err(ProfcastError::Parse {
                    line: idx + 1,
                    message: "stack has no frames".to_owned(),
                });
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
            frame_intern: interner.frames,
            samples,
            value_kinds: vec![ValueKind {
                kind: "samples".to_owned(),
                unit: "count".to_owned(),
            }],
        })
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
        let labels: Vec<_> = profile
            .frame_intern
            .iter()
            .map(|f| f.raw.as_str())
            .collect();
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
        let labels: Vec<_> = profile
            .frame_intern
            .iter()
            .map(|f| f.raw.as_str())
            .collect();
        assert_eq!(labels, ["Foo::bar(int, int)", "baz"]);
        assert_eq!(profile.samples[0].values, vec![7]);
    }

    #[test]
    fn tolerates_stray_semicolons() {
        let profile = profile_of("a;;b; 4\n");
        let labels: Vec<_> = profile
            .frame_intern
            .iter()
            .map(|f| f.raw.as_str())
            .collect();
        assert_eq!(labels, ["a", "b"]);
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
}
