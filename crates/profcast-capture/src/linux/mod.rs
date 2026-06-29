//! Linux capture backend built on `perf_event_open(2)`.
//!
//! [`PerfSource`] opens a software cpu-clock sampling event per thread of the
//! target, drains their ring buffers over the sampling window, and folds the
//! call-chains into an aggregated [`Profile`]. Stacks are symbolized
//! best-effort against the target's loaded binaries.

mod launch;
mod perf;
mod symbolize;

use std::collections::HashMap;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use profcast_core::{
    ProfcastError, Result,
    capture::{CaptureSpec, Source, Target},
    model::{Frame, FrameId, Profile, Sample, ValueKind},
};

use launch::Launched;
use perf::Counter;
use symbolize::Symbolizer;

/// Default sampling window when the caller does not specify a duration and the
/// target is not a separate process we can wait on.
const DEFAULT_DURATION: Duration = Duration::from_secs(10);

/// How often the drain loop wakes to copy samples out of the ring buffers.
const DRAIN_INTERVAL: Duration = Duration::from_millis(100);

/// The `perf_event_open`-based sampling profiler.
#[derive(Debug, Default)]
pub struct PerfSource;

impl PerfSource {
    /// Creates a new backend handle.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Source for PerfSource {
    fn name(&self) -> &'static str {
        "perf"
    }

    fn available(&self) -> bool {
        // Probe by opening (and immediately dropping) a sampling event on our
        // own thread. This catches a kernel without perf support and a sandbox
        // that blocks the syscall, without any lasting effect.
        match Counter::open(0, 99, false) {
            Ok(_) => true,
            Err(err) => {
                tracing::debug!(%err, "perf backend probe failed");
                false
            }
        }
    }

    fn capture(&self, spec: &CaptureSpec) -> Result<Profile> {
        if spec.frequency_hz == 0 {
            return Err(ProfcastError::Capture(
                "sampling frequency must be greater than zero".to_owned(),
            ));
        }

        // A launched program is held just before exec so we can attach events
        // first; for it sampling auto-starts at exec (`enable_on_exec`), whereas
        // an existing process is enabled immediately.
        let mut launched: Option<Launched> = None;
        let (pid, wait_for_exit, enable_on_exec) = match &spec.target {
            Target::Current => (std::process::id(), false, false),
            Target::Pid(pid) => (*pid, true, false),
            Target::Command(argv) => {
                let child = Launched::spawn(argv)?;
                let pid = child.pid;
                launched = Some(child);
                (pid, true, true)
            }
            other => {
                return Err(ProfcastError::Unsupported(format!(
                    "perf backend does not support target {other:?}"
                )));
            }
        };

        // If opening events fails, dropping `launched` kills the held child.
        let counters = open_counters(pid, spec.frequency_hz, enable_on_exec)?;

        if enable_on_exec {
            // Let the child exec; the events turn on at exec automatically.
            if let Some(child) = launched.as_mut() {
                child.release();
            }
        } else {
            for counter in &counters {
                counter.enable()?;
            }
        }
        tracing::info!(pid, threads = counters.len(), "sampling started");

        let deadline = match spec.duration {
            Some(duration) => Instant::now().checked_add(duration),
            None if wait_for_exit => None,
            None => Instant::now().checked_add(DEFAULT_DURATION),
        };

        let counts = sample_loop(&counters, deadline, || {
            launched.as_mut().map_or_else(
                || wait_for_exit && !process_is_alive(pid),
                Launched::has_exited,
            )
        });

        // Build the profile while the (now-resolved) target's maps are still
        // readable; `launched` is dropped afterwards, reaping the child.
        tracing::info!(stacks = counts.len(), "sampling stopped");
        build_profile(pid, &counts)
    }
}

/// Opens a sampling event per existing thread of `pid`. Threads that vanish
/// between enumeration and open are skipped; an empty result is an error.
fn open_counters(pid: u32, frequency_hz: u32, enable_on_exec: bool) -> Result<Vec<Counter>> {
    let tids = read_thread_ids(pid)?;
    let mut counters = Vec::with_capacity(tids.len());
    for tid in tids {
        match Counter::open(tid, frequency_hz, enable_on_exec) {
            Ok(counter) => counters.push(counter),
            Err(err) => tracing::debug!(tid, %err, "skipping thread that could not be opened"),
        }
    }
    if counters.is_empty() {
        return Err(ProfcastError::Capture(format!(
            "could not open any sampling events for process {pid}"
        )));
    }
    Ok(counters)
}

/// Drains the counters every [`DRAIN_INTERVAL`] until `deadline` passes or
/// `is_done` reports the target has finished, then disables and drains a final
/// time. Returns leaf-first call-chains aggregated to sample counts.
fn sample_loop(
    counters: &[Counter],
    deadline: Option<Instant>,
    mut is_done: impl FnMut() -> bool,
) -> HashMap<Vec<u64>, i64> {
    let mut counts: HashMap<Vec<u64>, i64> = HashMap::new();
    loop {
        thread::sleep(DRAIN_INTERVAL);
        for counter in counters {
            counter.drain(|ips| record_sample(&mut counts, ips));
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        if is_done() {
            break;
        }
    }

    for counter in counters {
        if let Err(err) = counter.disable() {
            tracing::debug!(%err, "failed to disable counter during teardown");
        }
    }
    // A final drain to capture anything buffered after the last interval.
    for counter in counters {
        counter.drain(|ips| record_sample(&mut counts, ips));
    }
    counts
}

/// Accumulates one leaf-first call-chain into the aggregated sample counts.
fn record_sample(counts: &mut HashMap<Vec<u64>, i64>, ips: &[u64]) {
    let slot = counts.entry(ips.to_vec()).or_insert(0);
    *slot = slot.saturating_add(1);
}

/// Reads the thread ids of `pid` from `/proc/<pid>/task`.
fn read_thread_ids(pid: u32) -> Result<Vec<u32>> {
    let dir = format!("/proc/{pid}/task");
    let entries = std::fs::read_dir(&dir).map_err(|err| {
        ProfcastError::Capture(format!(
            "cannot read threads of process {pid} ({dir}): {err}"
        ))
    })?;
    let mut tids: Vec<u32> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().and_then(|n| n.parse().ok()))
        .collect();
    tids.sort_unstable();
    if tids.is_empty() {
        return Err(ProfcastError::Capture(format!(
            "process {pid} has no readable threads"
        )));
    }
    Ok(tids)
}

/// Whether `/proc/<pid>` still exists.
fn process_is_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Folds aggregated, leaf-first call-chains into a [`Profile`], symbolizing and
/// interning frames. Stacks are reversed to the model's root-first order.
fn build_profile(pid: u32, counts: &HashMap<Vec<u64>, i64>) -> Result<Profile> {
    // For Target::Current, std::process::id() is our own pid; the symbolizer
    // treats it like any other /proc entry.
    let mut symbolizer = Symbolizer::new(pid);

    let mut frames: Vec<Frame> = Vec::new();
    let mut frame_ids: HashMap<u64, FrameId> = HashMap::new();
    let mut samples: Vec<Sample> = Vec::with_capacity(counts.len());

    for (leaf_first, &count) in counts {
        let mut stack = Vec::with_capacity(leaf_first.len());
        // Reverse to root-first (stack[0] is outermost) per the data model.
        for &ip in leaf_first.iter().rev() {
            let id = if let Some(id) = frame_ids.get(&ip).copied() {
                id
            } else {
                let next = u32::try_from(frames.len())
                    .map_err(|_| ProfcastError::Capture("too many distinct frames".to_owned()))?;
                let id = FrameId(next);
                frames.push(symbolizer.resolve(ip));
                frame_ids.insert(ip, id);
                id
            };
            stack.push(id);
        }
        samples.push(Sample {
            stack,
            values: vec![count],
        });
    }

    let profile = Profile {
        frames,
        samples,
        value_kinds: vec![ValueKind {
            kind: "samples".to_owned(),
            unit: "count".to_owned(),
        }],
    };
    profile.validate()?;
    Ok(profile)
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// End-to-end smoke test: profile ourselves while a thread burns CPU, and
    /// assert we captured at least one sample. Ignored by default because it
    /// needs a kernel and `perf_event_paranoid` low enough to sample our own
    /// process - run explicitly with `cargo test -- --ignored`.
    #[test]
    #[ignore = "requires perf_event_open access; run with --ignored"]
    fn captures_self_under_load() {
        let source = PerfSource::new();
        if !source.available() {
            eprintln!("perf_event_open unavailable; skipping");
            return;
        }

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let mut x = 0_u64;
            while !worker_stop.load(Ordering::Relaxed) {
                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                std::hint::black_box(x);
            }
        });

        let spec = CaptureSpec {
            target: Target::Current,
            frequency_hz: 997,
            duration: Some(Duration::from_millis(400)),
        };
        let profile = source.capture(&spec).expect("capture should succeed");

        stop.store(true, Ordering::Relaxed);
        worker.join().expect("worker should join");

        assert!(
            !profile.samples.is_empty(),
            "expected at least one sample while burning CPU"
        );
        profile.validate().expect("profile should be valid");
    }
}
