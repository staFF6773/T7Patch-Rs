//! A bounded view of the on-disk journal; no game calls or packet-thread work.
use std::{
    collections::VecDeque,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};

const MAX_BYTES: u64 = 256 * 1024;
const MAX_EVENTS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Rejected,
    Limited,
    Diagnostics,
}
impl Filter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Rejected,
            Self::Rejected => Self::Limited,
            Self::Limited => Self::Diagnostics,
            Self::Diagnostics => Self::All,
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Rejected => "Rejected",
            Self::Limited => "Rate limits",
            Self::Diagnostics => "Diagnostics",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    time: String,
    category: Filter,
    label: &'static str,
    count: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    entries: VecDeque<Entry>,
    pub notice: String,
    pub available: bool,
}
impl Snapshot {
    fn push(&mut self, entry: Entry) {
        if self.entries.len() == MAX_EVENTS {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    pub fn summary(&self) -> String {
        let total = |category| {
            self.entries
                .iter()
                .filter(|entry| entry.category == category)
                .fold(0u64, |sum, entry| sum.saturating_add(entry.count))
        };
        format!(
            "Recent totals: {} rejected · {} rate-limited · {} diagnostics",
            total(Filter::Rejected),
            total(Filter::Limited),
            total(Filter::Diagnostics)
        )
    }

    pub fn text(&self, filter: Filter) -> String {
        let lines: Vec<_> = self
            .entries
            .iter()
            .rev()
            .filter(|entry| filter == Filter::All || entry.category == filter)
            .map(|entry| {
                format!(
                    "{} | {} | {}: {}",
                    entry.time,
                    entry.category.title(),
                    entry.label,
                    entry.count
                )
            })
            .collect();
        if lines.is_empty() {
            "No recorded events for this filter.".into()
        } else {
            lines.join("\r\n")
        }
    }
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_ascii_whitespace()
        .find_map(|part| part.strip_prefix(key))
}

fn parse(bytes: &[u8], skip_first: bool) -> Snapshot {
    let mut snapshot = Snapshot::default();
    let Some(end) = bytes.iter().rposition(|&b| b == b'\n') else {
        return snapshot;
    };
    // Ignore a partial first/last line; a writer may still be appending it.
    let start = if skip_first {
        bytes.iter().position(|&b| b == b'\n').unwrap() + 1
    } else {
        0
    };
    for line in bytes[start..=end].split(|&b| b == b'\n') {
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        let time = if let Some(utc) = field(line, "utc=").filter(|s| {
            s.len() == 20
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || b"-:TZ".contains(&b))
        }) {
            utc.to_owned()
        } else if let Some(tick) = field(line, "tick=").and_then(|s| s.parse::<u64>().ok()) {
            let seconds = tick / 1000;
            format!(
                "Boot +{:02}:{:02}:{:02}",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60
            )
        } else {
            continue;
        };
        let mut push = |category, label, count| {
            snapshot.push(Entry {
                time: time.clone(),
                category,
                label,
                count,
            });
        };
        let has = |token| line.split_ascii_whitespace().any(|part| part == token);
        if has("network-guard") {
            for (key, category, label) in [
                ("envelope=", Filter::Rejected, "Invalid message descriptors"),
                ("instant=", Filter::Rejected, "Instant-message checks"),
                ("p2p=", Filter::Rejected, "P2P buffer checks"),
                ("lobby=", Filter::Rejected, "Lobby-message checks"),
                ("social_limited=", Filter::Limited, "Social actions"),
                ("instant_limited=", Filter::Limited, "Instant messages"),
                ("p2p_limited=", Filter::Limited, "P2P control traffic"),
                ("oob_limited=", Filter::Limited, "Connectionless responses"),
                (
                    "friends_refresh_failed=",
                    Filter::Diagnostics,
                    "Friend refresh retries",
                ),
            ] {
                if let Some(count) = field(line, key).and_then(|value| value.parse::<u64>().ok()) {
                    if count != 0 {
                        push(category, label, count);
                    }
                }
            }
        } else if has("session-start") {
            push(Filter::Diagnostics, "Patch initialization started", 1);
        } else if has("reader-diagnostic") && has("action=observe") {
            push(Filter::Diagnostics, "Reader observation (not a block)", 1);
        } else if has("unhandled-exception") || has("fatal-script") {
            push(Filter::Diagnostics, "Game error (see full journal)", 1);
        }
    }
    snapshot
}

fn load(path: &Path) -> std::io::Result<Snapshot> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let offset = length.saturating_sub(MAX_BYTES);
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES).read_to_end(&mut bytes)?;
    let mut snapshot = parse(&bytes, offset != 0);
    snapshot.available = true;
    snapshot.notice = "Latest 200 events from up to 256 KiB · UTC / legacy boot time".into();
    Ok(snapshot)
}

pub struct History {
    pub snapshot: Arc<Mutex<Snapshot>>,
    pub path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl History {
    pub fn start(path: PathBuf, enabled: bool) -> std::io::Result<Self> {
        let initial = if enabled {
            Snapshot {
                notice: "Reading protection journal…".into(),
                ..Snapshot::default()
            }
        } else {
            let mut preview = parse(b"tick=1000 utc=2026-09-15T12:00:00Z network-guard envelope=2 social_limited=3 friends_refresh_failed=1\ntick=2000 utc=2026-09-15T12:00:10Z reader-diagnostic action=observe\n", false);
            preview.notice = "Preview · sample events (not live) · journal reading disabled".into();
            preview
        };
        let snapshot = Arc::new(Mutex::new(initial));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = if enabled {
            let path = path.clone();
            let snapshot = snapshot.clone();
            let stop = stop.clone();
            Some(
                std::thread::Builder::new()
                    .name("t7patch-history".into())
                    .spawn(move || {
                        while !stop.load(Ordering::Acquire) {
                            let current = load(&path).unwrap_or_else(|error| Snapshot {
                                notice: if error.kind() == std::io::ErrorKind::NotFound {
                                    "No journal beside the launcher yet.".into()
                                } else {
                                    format!("Cannot read journal: {error}")
                                },
                                ..Snapshot::default()
                            });
                            *snapshot.lock().unwrap_or_else(|e| e.into_inner()) = current;
                            for _ in 0..20 {
                                if stop.load(Ordering::Acquire) {
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(100));
                            }
                        }
                    })?,
            )
        } else {
            None
        };
        Ok(Self {
            snapshot,
            path,
            stop,
            thread,
        })
    }
}
impl Drop for History {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_rejections_limits_and_observations_without_exposing_raw_fields() {
        let snapshot = parse(b"tick=1000 utc=2026-09-15T12:30:00Z thread=9 network-guard envelope=2 instant=3 p2p=0 lobby=4 social_limited=5 instant_limited=0 p2p_limited=6 oob_limited=7 friends_refresh_failed=8 password=secret\ntick=2000 reader-diagnostic action=observe command=secret\n", false);
        assert_eq!(
            snapshot.summary(),
            "Recent totals: 9 rejected · 18 rate-limited · 9 diagnostics"
        );
        assert!(!snapshot.text(Filter::All).contains("secret"));
        assert!(!snapshot
            .text(Filter::Rejected)
            .contains("Reader observation"));
        assert!(snapshot.text(Filter::Diagnostics).contains("not a block"));
        assert!(snapshot
            .text(Filter::Limited)
            .contains("2026-09-15T12:30:00Z"));
    }

    #[test]
    fn ignores_partial_lines_bad_counts_and_caps_history() {
        assert!(parse(b"tick=1 network-guard envelope=1", false)
            .entries
            .is_empty());
        assert!(parse(b"tick=1 network-guard envelope=1\n", true)
            .entries
            .is_empty());
        let mut input =
            "cut line\ntick=1 network-guard envelope=-1 lobby=18446744073709551616\n".to_owned();
        for tick in 0..250 {
            input.push_str(&format!("tick={tick} network-guard envelope=1\n"));
        }
        input.push_str("tick=2 network-guard envelope=999");
        let snapshot = parse(input.as_bytes(), true);
        assert_eq!(snapshot.entries.len(), MAX_EVENTS);
        assert_eq!(
            snapshot.summary(),
            "Recent totals: 200 rejected · 0 rate-limited · 0 diagnostics"
        );
    }

    #[test]
    fn reload_handles_append_truncation_and_replacement() {
        let path =
            std::env::temp_dir().join(format!("t7patch-history-test-{}.log", std::process::id()));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        std::fs::write(&path, b"tick=1 network-guard envelope=2\n").unwrap();
        assert_eq!(load(&path).unwrap().entries[0].count, 2);
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"tick=2 network-guard lobby=3\n")
            .unwrap();
        assert_eq!(load(&path).unwrap().entries.len(), 2);
        std::fs::write(&path, b"").unwrap();
        assert!(load(&path).unwrap().entries.is_empty());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"tick=3 network-guard p2p=9\n").unwrap();
        assert_eq!(load(&path).unwrap().entries[0].count, 9);
        let mut large = vec![b'x'; MAX_BYTES as usize];
        large.extend_from_slice(b"\ntick=4 network-guard envelope=7\n");
        std::fs::write(&path, large).unwrap();
        let snapshot = load(&path).unwrap();
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].count, 7);
    }
}
