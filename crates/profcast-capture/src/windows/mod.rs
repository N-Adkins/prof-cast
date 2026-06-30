//! Windows capture backend built on thread suspension and `DbgHelp` stack walks.
//!
//! [`SamplingSource`] drives sampling entirely from user space: on a fixed
//! cadence it enumerates the target's threads, and for each one suspends it,
//! captures its register context, walks the call stack with `StackWalk64`, and
//! resumes it. The collected leaf-first call-chains are folded into an
//! aggregated [`Profile`] and symbolized through the same `DbgHelp` session that
//! powered the stack walks.
//!
//! Unlike the Linux `perf` backend there is no kernel ring buffer: the sampling
//! rate is realized by the drain loop's own timing, so `frequency_hz` becomes
//! the interval between full thread snapshots.

mod launch;
mod sample;
mod symbolize;

use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use profcast_core::{
    ProfcastError, Result,
    capture::{CaptureSpec, Source, Target},
    model::{Frame, FrameId, Profile, Sample, ValueKind},
};

use launch::Launched;
use sample::{ProcessHandle, sample_running_thread, thread_ids};
use symbolize::Symbolizer;

/// Default sampling window when the caller does not specify a duration and the
/// target is not a separate process we can wait on.
const DEFAULT_DURATION: Duration = Duration::from_secs(10);

/// Floor on the snapshot interval, so an absurd `frequency_hz` cannot spin the
/// drain loop into a busy-wait that starves the threads it keeps suspending.
const MIN_INTERVAL: Duration = Duration::from_millis(1);

/// Fallback snapshot interval if the requested frequency cannot be turned into a
/// duration (only possible for a zero frequency, which is rejected earlier).
const FALLBACK_INTERVAL: Duration = Duration::from_millis(10);

/// How long to reuse a cached thread-id list before re-enumerating. A full
/// thread enumeration is a system-wide `CreateToolhelp32Snapshot` costing tens of
/// milliseconds, so doing it every tick would dominate the loop; the target's
/// thread set is near-static, so brief staleness only delays a new thread's
/// first sample by at most this interval.
const THREAD_LIST_REFRESH: Duration = Duration::from_millis(500);

/// The thread-suspension sampling profiler.
#[derive(Debug, Default)]
pub struct SamplingSource;

impl SamplingSource {
    /// Creates a new backend handle.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Source for SamplingSource {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn available(&self) -> bool {
        // This backend is compiled only for x86-64 Windows (see the module gate
        // in lib.rs); Tool Help and DbgHelp are always present there.
        true
    }

    fn capture(&self, spec: &CaptureSpec) -> Result<Profile> {
        if spec.frequency_hz == 0 {
            return Err(ProfcastError::Capture(
                "sampling frequency must be greater than zero".to_owned(),
            ));
        }

        // A launched program is created suspended so we can attach DbgHelp before
        // it runs; an existing process (or ourselves) is sampled as-is.
        let mut launched: Option<Launched> = None;
        let (process, pid, wait_for_exit) = match &spec.target {
            Target::Current => (ProcessHandle::current(), sample::current_pid(), false),
            Target::Pid(pid) => (ProcessHandle::open(*pid)?, *pid, true),
            Target::Command(argv) => {
                let child = Launched::spawn(argv)?;
                let handle = ProcessHandle::borrowed(child.process_handle());
                let pid = child.pid;
                launched = Some(child);
                (handle, pid, true)
            }
            other => {
                return Err(ProfcastError::Unsupported(format!(
                    "windows backend does not support target {other:?}"
                )));
            }
        };

        // Initialize symbolization (and the function tables `StackWalk64` needs)
        // before the child is released, so its startup is covered. A launched
        // child is still suspended here, so we cannot enumerate its modules yet
        // (`invade = false`); the drain loop refreshes them once it is resumed.
        let invade = launched.is_none();
        let mut symbolizer = Symbolizer::new(process.raw(), invade)?;

        // Baseline thread cycles before resuming, so a target that runs for only a
        // tick or two is walked on the first tick instead of being spent
        // establishing baselines. A suspended child reads ~0 here, so its post-resume
        // work counts immediately.
        let initial_cycles = sample::snapshot_thread_cycles(pid);

        if let Some(child) = launched.as_mut() {
            child.resume()?;
        }
        tracing::info!(pid, "sampling started");

        let interval = snapshot_interval(spec.frequency_hz);
        let deadline = match spec.duration {
            Some(duration) => Instant::now().checked_add(duration),
            None if wait_for_exit => None,
            None => Instant::now().checked_add(DEFAULT_DURATION),
        };

        let counts = sample_loop(
            process.raw(),
            pid,
            &mut symbolizer,
            interval,
            deadline,
            initial_cycles,
            || {
                launched.as_ref().map_or_else(
                    || wait_for_exit && process.has_exited(),
                    Launched::has_exited,
                )
            },
        );

        tracing::info!(stacks = counts.len(), "sampling stopped");
        build_profile(&symbolizer, &counts)
    }
}

/// Turns the requested frequency into the delay between thread snapshots,
/// clamped so the loop cannot starve the target it keeps suspending.
fn snapshot_interval(frequency_hz: u32) -> Duration {
    Duration::from_secs(1)
        .checked_div(frequency_hz)
        .unwrap_or(FALLBACK_INTERVAL)
        .max(MIN_INTERVAL)
}

/// Snapshots every thread of `pid` once per `interval` until `deadline` passes
/// or `is_done` reports the target has finished. Returns leaf-first call-chains
/// aggregated to sample counts.
fn sample_loop(
    process: sample::Handle,
    pid: u32,
    symbolizer: &mut Symbolizer,
    interval: Duration,
    deadline: Option<Instant>,
    initial_cycles: HashMap<u32, u64>,
    mut is_done: impl FnMut() -> bool,
) -> HashMap<Vec<u64>, i64> {
    let mut counts: HashMap<Vec<u64>, i64> = HashMap::new();
    let mut stack: Vec<u64> = Vec::new();
    // Per-thread CPU cycle count at the previous tick, so we can tell which
    // threads actually ran (and should be sampled) from idle ones parked in a
    // wait. Seeded from a pre-resume baseline (see `snapshot_thread_cycles`) and
    // cleared of departed threads each tick to bound its size.
    let mut last_cycles: HashMap<u32, u64> = initial_cycles;
    // Cached thread-id list and when it was last refreshed (see THREAD_LIST_REFRESH).
    let mut tids: Vec<u32> = Vec::new();
    let mut last_thread_refresh: Option<Instant> = None;
    let mut ticks: u64 = 0;
    loop {
        let tick_start = Instant::now();
        // Keep DbgHelp's module list current so newly-loaded DLLs can both be
        // walked and, later, symbolized; the call self-throttles.
        symbolizer.refresh_modules();
        // Enumerating threads means a system-wide ToolHelp snapshot (tens of ms);
        // the target's thread set changes rarely, so refresh it only occasionally
        // and reuse the cached list in between.
        if last_thread_refresh.is_none_or(|t| tick_start.duration_since(t) >= THREAD_LIST_REFRESH) {
            tids = thread_ids(pid);
            last_thread_refresh = Some(tick_start);
        }
        let mut sampled = 0_usize;
        let mut current_cycles: HashMap<u32, u64> = HashMap::with_capacity(tids.len());
        for tid in &tids {
            stack.clear();
            let prev = last_cycles.get(tid).copied();
            if let Some(cycles) = sample_running_thread(process, *tid, prev, &mut stack) {
                current_cycles.insert(*tid, cycles);
                if !stack.is_empty() {
                    record_sample(&mut counts, &stack);
                    sampled = sampled.saturating_add(1);
                }
            }
        }
        // Carry only still-live threads' baselines into the next tick.
        last_cycles = current_cycles;
        ticks = ticks.saturating_add(1);
        tracing::trace!(
            tick = ticks,
            threads = tids.len(),
            sampled,
            "thread snapshot taken"
        );
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        if is_done() {
            break;
        }
        // Hold the requested cadence: sleep only the remainder of this tick's
        // interval, so per-tick work doesn't stretch the sampling period.
        if let Some(remaining) = interval.checked_sub(tick_start.elapsed()) {
            thread::sleep(remaining);
        }
    }
    tracing::debug!(ticks, distinct_stacks = counts.len(), "drain loop finished");
    counts
}

/// Accumulates one leaf-first call-chain into the aggregated sample counts.
fn record_sample(counts: &mut HashMap<Vec<u64>, i64>, ips: &[u64]) {
    let slot = counts.entry(ips.to_vec()).or_insert(0);
    *slot = slot.saturating_add(1);
}

/// Folds aggregated, leaf-first call-chains into a [`Profile`], symbolizing and
/// interning frames. Stacks are reversed to the model's root-first order.
fn build_profile(symbolizer: &Symbolizer, counts: &HashMap<Vec<u64>, i64>) -> Result<Profile> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_interval_is_reciprocal_and_floored() {
        // 100 Hz -> 10 ms.
        assert_eq!(snapshot_interval(100), Duration::from_millis(10));
        // 1 Hz -> 1 s.
        assert_eq!(snapshot_interval(1), Duration::from_secs(1));
        // An absurd rate is floored, not turned into a busy-wait.
        assert_eq!(snapshot_interval(u32::MAX), MIN_INTERVAL);
    }

    /// End-to-end smoke test: profile ourselves while a thread burns CPU and
    /// assert we captured at least one sample. Ignored by default because it
    /// suspends and walks live threads; run explicitly with
    /// `cargo test -- --ignored`.
    #[test]
    #[ignore = "suspends live threads; run with --ignored"]
    fn captures_self_under_load() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let source = SamplingSource::new();
        if !source.available() {
            eprintln!("windows sampling backend unavailable; skipping");
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
