//! Opt-in, sampled timings. Hook/exception threads only update atomics; the worker writes reports.
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

static ENABLED: AtomicBool = AtomicBool::new(false);
static METRICS: Mutex<Vec<&'static Metric>> = Mutex::new(Vec::new());
pub static BOUNDED_STRING: Metric = Metric::new("memory.bounded_string");
pub static GAME_STRING: Metric = Metric::new("memory.game_string");
pub static EXCEPTIONS: Metric = Metric::new("exceptions.handle");
pub static LOBBY_EXCEPTIONS: Metric = Metric::new("exceptions.lobby_sentinel");
pub static FRIENDS_REFRESH: Metric = Metric::new("steam.friends_refresh");
pub static CONFIG_POLL: Metric = Metric::new("worker.config_poll");
pub static INTEGRITY_MAINTAIN: Metric = Metric::new("worker.integrity");
pub static IDLE_UPDATE: Metric = Metric::new("game.idle_update");
const SAMPLE_EVERY: u64 = 1024;
static PENDING: PendingCalls<256> = PendingCalls::new();

pub struct Metric {
    name: &'static str,
    calls: AtomicU64,
    samples: AtomicU64,
    nanos: AtomicU64,
    max_nanos: AtomicU64,
}

impl Metric {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            calls: AtomicU64::new(0),
            samples: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
            max_nanos: AtomicU64::new(0),
        }
    }

    #[inline]
    pub fn enter(&self) -> Option<Sample<'_>> {
        if !ENABLED.load(Ordering::Relaxed) {
            return None;
        }
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        if call % SAMPLE_EVERY != 0 {
            return None;
        }
        Some(Sample {
            metric: self,
            started: Instant::now(),
        })
    }

    pub fn register(&'static self) {
        let mut metrics = METRICS.lock().unwrap_or_else(|e| e.into_inner());
        if !metrics.iter().any(|metric| std::ptr::eq(*metric, self)) {
            metrics.push(self);
        }
    }

    /// Every call is tracked when profiling is enabled, including unsampled calls
    /// that never return. No allocation, mutex, or I/O on the calling thread.
    #[inline]
    pub fn track(&'static self) -> Option<Pending<'static>> {
        if !ENABLED.load(Ordering::Relaxed) {
            return None;
        }
        unsafe {
            PENDING.begin(
                self,
                windows_sys::Win32::System::Threading::GetCurrentThreadId(),
                windows_sys::Win32::System::SystemInformation::GetTickCount64(),
            )
        }
    }
}

struct PendingSlot {
    // 0 = free, 1 = being filled, otherwise a unique even publication token.
    owner: AtomicU64,
    metric: AtomicPtr<Metric>,
    thread: AtomicU32,
    since: AtomicU64,
}
impl PendingSlot {
    const fn new() -> Self {
        Self {
            owner: AtomicU64::new(0),
            metric: AtomicPtr::new(std::ptr::null_mut()),
            thread: AtomicU32::new(0),
            since: AtomicU64::new(0),
        }
    }
}
struct PendingCalls<const N: usize> {
    slots: [PendingSlot; N],
    next: AtomicU64,
    overflow: AtomicU64,
}
impl<const N: usize> PendingCalls<N> {
    const fn new() -> Self {
        Self {
            slots: [const { PendingSlot::new() }; N],
            next: AtomicU64::new(2),
            overflow: AtomicU64::new(0),
        }
    }

    fn begin(&self, metric: &'static Metric, thread: u32, since: u64) -> Option<Pending<'_>> {
        for offset in 0..N {
            let slot = &self.slots[(thread as usize + offset) % N];
            if slot
                .owner
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                continue;
            }
            let token = self.next.fetch_add(2, Ordering::Relaxed);
            if token < 2 {
                slot.owner.store(0, Ordering::Release);
                break;
            }
            slot.metric
                .store(std::ptr::from_ref(metric).cast_mut(), Ordering::Relaxed);
            slot.thread.store(thread, Ordering::Relaxed);
            slot.since.store(since, Ordering::Relaxed);
            slot.owner.store(token, Ordering::Release);
            return Some(Pending { slot });
        }
        self.overflow.fetch_add(1, Ordering::Relaxed);
        None
    }

    fn append(&self, report: &mut String, now: u64) {
        use std::fmt::Write;
        for slot in &self.slots {
            let token = slot.owner.load(Ordering::Acquire);
            if token < 2 {
                continue;
            }
            let metric = slot.metric.load(Ordering::Relaxed);
            let thread = slot.thread.load(Ordering::Relaxed);
            let since = slot.since.load(Ordering::Relaxed);
            // A returned/reused slot must not combine two calls in one report.
            if slot.owner.load(Ordering::Acquire) != token || metric.is_null() {
                continue;
            }
            let age = now.saturating_sub(since);
            if age >= 2000 {
                // Only &'static Metric pointers are ever published in a slot.
                let name = unsafe { &*metric }.name;
                let _ = writeln!(report, "PENDING thread={thread} call={name} age_ms={age}");
            }
        }
        let dropped = self.overflow.swap(0, Ordering::Relaxed);
        if dropped != 0 {
            let _ = writeln!(
                report,
                "PENDING overflow={dropped} (tracking capacity exceeded)"
            );
        }
    }
}
pub struct Pending<'a> {
    slot: &'a PendingSlot,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.slot.owner.store(0, Ordering::Release);
    }
}

pub struct Sample<'a> {
    metric: &'a Metric,
    started: Instant,
}

impl Drop for Sample<'_> {
    fn drop(&mut self) {
        let nanos = self.started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.metric.nanos.fetch_add(nanos, Ordering::Relaxed);
        self.metric.max_nanos.fetch_max(nanos, Ordering::Relaxed);
        self.metric.samples.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct Reporter {
    file: File,
    started: Instant,
    last: Instant,
}

impl Reporter {
    pub fn start(config_path: &Path) -> Option<Self> {
        let directory = config_path.parent()?;
        if !directory.join("t7patch-profile.enabled").is_file() {
            return None;
        }
        let result = (|| -> std::io::Result<File> {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(directory.join("t7patch-profile.log"))?;
            writeln!(
                file,
                "\nT7 Patch profile {:?}: game={:?}, debug_assertions={}, sample_every={SAMPLE_EVERY}, ui_strings=direct-bounded-v1, hang_tracking=v1, lobby_reader=validated-v2",
                std::time::SystemTime::now(),
                crate::game_build::current_build(),
                cfg!(debug_assertions),
            )?;
            writeln!(file, "Approximate 5s windows. Timings are sampled, inclusive wall time (nested metrics overlap); max is the sampled maximum. Exception timings exclude OS dispatch/resume overhead.\nmetric calls calls/s samples avg_us max_us")?;
            file.flush()?;
            Ok(file)
        })();
        match result {
            Ok(file) => {
                BOUNDED_STRING.register();
                GAME_STRING.register();
                EXCEPTIONS.register();
                LOBBY_EXCEPTIONS.register();
                FRIENDS_REFRESH.register();
                CONFIG_POLL.register();
                INTEGRITY_MAINTAIN.register();
                IDLE_UPDATE.register();
                ENABLED.store(true, Ordering::Relaxed);
                let now = Instant::now();
                Some(Self {
                    file,
                    started: now,
                    last: now,
                })
            }
            Err(error) => {
                crate::memory::debug(&format!("Could not start profiling: {error}"));
                None
            }
        }
    }

    pub fn poll(&mut self) {
        if !ENABLED.load(Ordering::Relaxed) || self.last.elapsed() < Duration::from_secs(5) {
            return;
        }
        let now = Instant::now();
        let seconds = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        // Never perform formatting or file I/O from a hook or exception handler.
        let mut report = format!(
            "\nt={:.1}s window={seconds:.3}s\n",
            now.duration_since(self.started).as_secs_f64()
        );
        {
            use std::fmt::Write;
            let metrics = METRICS.lock().unwrap_or_else(|e| e.into_inner());
            for metric in metrics.iter() {
                let calls = metric.calls.swap(0, Ordering::Relaxed);
                let samples = metric.samples.swap(0, Ordering::Relaxed);
                let nanos = metric.nanos.swap(0, Ordering::Relaxed);
                let max = metric.max_nanos.swap(0, Ordering::Relaxed);
                if calls == 0 && samples == 0 {
                    continue;
                }
                let avg = if samples == 0 {
                    0.0
                } else {
                    nanos as f64 / samples as f64 / 1000.0
                };
                let _ = writeln!(
                    report,
                    "{} {calls} {:.1} {samples} {avg:.3} {:.3}",
                    metric.name,
                    calls as f64 / seconds,
                    max as f64 / 1000.0
                );
            }
        }
        PENDING.append(&mut report, unsafe {
            windows_sys::Win32::System::SystemInformation::GetTickCount64()
        });
        if let Err(error) = self
            .file
            .write_all(report.as_bytes())
            .and_then(|_| self.file.flush())
        {
            ENABLED.store(false, Ordering::Relaxed);
            crate::memory::debug(&format!("Profiling stopped after write failure: {error}"));
        }
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        ENABLED.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_calls_survive_report_windows_until_the_call_returns() {
        static OUTER: Metric = Metric::new("test.outer");
        static INNER: Metric = Metric::new("test.inner");
        let tracker = PendingCalls::<2>::new();
        let outer = tracker.begin(&OUTER, 42, 100).unwrap();
        let inner = tracker.begin(&INNER, 42, 200).unwrap();
        assert!(tracker.begin(&INNER, 99, 300).is_none()); // Bounded; never wait.
        let mut report = String::new();
        tracker.append(&mut report, 2500);
        assert!(report.contains("thread=42 call=test.outer age_ms=2400"));
        assert!(report.contains("thread=42 call=test.inner age_ms=2300"));
        assert!(report.contains("overflow=1"));
        drop(inner);
        let reused = tracker.begin(&INNER, 99, 2600).unwrap();
        report.clear();
        tracker.append(&mut report, 3000);
        assert!(report.contains("test.outer"));
        assert!(!report.contains("test.inner")); // No stale age after slot reuse.
        drop(reused);
        drop(outer);
        report.clear();
        tracker.append(&mut report, 10000);
        assert!(report.is_empty());
    }

    #[test]
    fn another_thread_can_report_an_inflight_call() {
        static METRIC: Metric = Metric::new("test.waiting_thread");
        let tracker = PendingCalls::<4>::new();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let barrier = &barrier;
            let tracker = &tracker;
            scope.spawn(move || {
                let _pending = tracker.begin(&METRIC, 123, 100).unwrap();
                barrier.wait();
                barrier.wait();
            });
            barrier.wait();
            let mut report = String::new();
            tracker.append(&mut report, 5100);
            // Release the worker even if an assertion subsequently fails.
            barrier.wait();
            assert!(report.contains("thread=123 call=test.waiting_thread age_ms=5000"));
        });
        let mut report = String::new();
        tracker.append(&mut report, 10000);
        assert!(report.is_empty());
    }

    #[test]
    fn profiling_is_opt_in_sampled_and_written_only_by_reporter() {
        let directory = std::env::temp_dir().join(format!(
            "t7patch-profile-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let config = directory.join("t7patch.conf");
        let log = directory.join("t7patch-profile.log");
        static METRIC: Metric = Metric::new("test.sampled_hook");
        METRIC.register();
        assert!(Reporter::start(&config).is_none());
        drop(METRIC.enter());
        assert_eq!(METRIC.calls.load(Ordering::Relaxed), 0);
        assert!(!log.exists());

        std::fs::write(directory.join("t7patch-profile.enabled"), b"").unwrap();
        let mut reporter = Reporter::start(&config).unwrap();
        for _ in 0..1025 {
            drop(METRIC.enter());
        }
        reporter.poll();
        assert!(!std::fs::read_to_string(&log).unwrap().contains(METRIC.name));
        reporter.last = Instant::now() - Duration::from_secs(6);
        reporter.poll();
        let report = std::fs::read_to_string(&log).unwrap();
        let row: Vec<_> = report
            .lines()
            .find(|line| line.starts_with(METRIC.name))
            .unwrap()
            .split_whitespace()
            .collect();
        assert_eq!(row[1], "1025");
        assert_eq!(row[3], "2");
        reporter.last = Instant::now() - Duration::from_secs(6);
        reporter.poll();
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap()
                .matches(METRIC.name)
                .count(),
            1
        );
        drop(reporter);
        drop(METRIC.enter());
        assert_eq!(METRIC.calls.load(Ordering::Relaxed), 0);
    }
}
