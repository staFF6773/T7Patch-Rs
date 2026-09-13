//! Bounded admission of control traffic, shared by Campaign, MP and Zombies.
//! These are load-shedding budgets, not authentication or a gameplay packet limiter.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

const PEERS: usize = 256;
const PEER_IDLE_MS: u64 = 30_000;
const UNIT: u64 = 1000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    Steam(u64),
    // Do not hash or compare native structure padding. These addresses are not authenticated.
    Address {
        ip: [u8; 4],
        port: u16,
        kind: i32,
        local_net_id: i32,
    },
}

#[derive(Clone, Copy)]
pub enum Channel {
    Social,
    Instant,
    P2pControl,
    Connectionless,
}

#[derive(Clone, Copy)]
struct Budget {
    packets: u64,
    bytes: u64,
}

#[derive(Clone, Copy)]
struct Policy {
    peer: Budget,
    global: Budget,
}

// Per-second rates, with two seconds of burst credit. Separate pools prevent
// invitation floods from consuming lobby/P2P admission credit.
const POLICIES: [Policy; 4] = [
    Policy {
        peer: Budget {
            packets: 8,
            bytes: 0,
        },
        global: Budget {
            packets: 128,
            bytes: 0,
        },
    },
    Policy {
        peer: Budget {
            packets: 128,
            bytes: 256 * 1024,
        },
        global: Budget {
            packets: 4096,
            bytes: 8 * 1024 * 1024,
        },
    },
    Policy {
        peer: Budget {
            packets: 256,
            bytes: 1024 * 1024,
        },
        global: Budget {
            packets: 8192,
            bytes: 32 * 1024 * 1024,
        },
    },
    Policy {
        peer: Budget {
            packets: 128,
            bytes: 256 * 1024,
        },
        global: Budget {
            packets: 4096,
            bytes: 8 * 1024 * 1024,
        },
    },
];

#[derive(Clone, Copy)]
struct Bucket {
    packets: u64,
    bytes: u64,
    updated: u64,
}
impl Bucket {
    const fn new(rate: Budget, now: u64) -> Self {
        Self {
            packets: rate.packets * 2 * UNIT,
            bytes: rate.bytes * 2 * UNIT,
            updated: now,
        }
    }
    fn take(&mut self, rate: Budget, bytes: u32, now: u64) -> bool {
        let elapsed = now.saturating_sub(self.updated).min(2000);
        self.updated = self.updated.max(now);
        self.packets = (self.packets + elapsed * rate.packets).min(rate.packets * 2 * UNIT);
        self.bytes = (self.bytes + elapsed * rate.bytes).min(rate.bytes * 2 * UNIT);
        let cost = u64::from(bytes) * UNIT;
        if self.packets < UNIT || (rate.bytes != 0 && self.bytes < cost) {
            return false;
        }
        self.packets -= UNIT;
        if rate.bytes != 0 {
            self.bytes -= cost;
        }
        true
    }
}

#[derive(Clone, Copy)]
struct Entry {
    peer: Peer,
    bucket: Bucket,
    last_seen: u64,
}

struct Gate {
    policy: Policy,
    global: Bucket,
    peers: [Option<Entry>; PEERS],
}
impl Gate {
    const fn new(policy: Policy) -> Self {
        Self {
            policy,
            global: Bucket::new(policy.global, 0),
            peers: [None; PEERS],
        }
    }
    fn allow(&mut self, peer: Peer, bytes: u32, now: u64) -> bool {
        // Charge attempts globally before scanning the bounded table. Even denied
        // peers and changing/spoofed identities cannot cause unbounded table work.
        if !self.global.take(self.policy.global, bytes, now) {
            return false;
        }
        let mut vacant = None;
        for (index, slot) in self.peers.iter_mut().enumerate() {
            match slot {
                Some(entry) if entry.peer == peer => {
                    entry.last_seen = entry.last_seen.max(now);
                    return entry.bucket.take(self.policy.peer, bytes, now);
                }
                Some(entry) if now.saturating_sub(entry.last_seen) >= PEER_IDLE_MS => {
                    vacant.get_or_insert(index);
                }
                None => {
                    vacant.get_or_insert(index);
                }
                _ => {}
            }
        }
        // Never evict a live peer to give a rotating identity fresh burst credit.
        let Some(index) = vacant else {
            return false;
        };
        let mut entry = Entry {
            peer,
            bucket: Bucket::new(self.policy.peer, now),
            last_seen: now,
        };
        let allowed = entry.bucket.take(self.policy.peer, bytes, now);
        self.peers[index] = Some(entry);
        allowed
    }
}

static GATES: [Mutex<Gate>; 4] = [
    Mutex::new(Gate::new(POLICIES[0])),
    Mutex::new(Gate::new(POLICIES[1])),
    Mutex::new(Gate::new(POLICIES[2])),
    Mutex::new(Gate::new(POLICIES[3])),
];

#[derive(Clone, Copy)]
pub enum Event {
    Envelope,
    Instant,
    P2p,
    Lobby,
    SocialRate,
    InstantRate,
    P2pRate,
    ConnectionlessRate,
    FriendsRetry,
}
static EVENTS: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];

pub fn record(event: Event) {
    EVENTS[event as usize].fetch_add(1, Ordering::Relaxed);
}

pub fn allow(channel: Channel, peer: Peer, bytes: u32) -> bool {
    let now = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
    let allowed = GATES[channel as usize]
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .allow(peer, bytes, now);
    if !allowed {
        record(match channel {
            Channel::Social => Event::SocialRate,
            Channel::Instant => Event::InstantRate,
            Channel::P2pControl => Event::P2pRate,
            Channel::Connectionless => Event::ConnectionlessRate,
        });
    }
    allowed
}

/// Called only by the existing maintenance worker, at most once every 10 seconds.
/// No packet contents, addresses, identities or passwords are recorded.
pub fn report() {
    let counts = std::array::from_fn::<_, 9, _>(|i| EVENTS[i].swap(0, Ordering::Relaxed));
    if counts.iter().any(|&count| count != 0) {
        crate::diagnostics::event(format_args!(
            "network-guard envelope={} instant={} p2p={} lobby={} social_limited={} instant_limited={} p2p_limited={} oob_limited={} friends_refresh_failed={}",
            counts[0], counts[1], counts[2], counts[3], counts[4], counts[5], counts[6], counts[7], counts[8],
        ));
    }
    crate::packets::report_reader_diagnostics();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_refill_bytes_and_clock_rollback() {
        let rate = Budget {
            packets: 2,
            bytes: 100,
        };
        let mut bucket = Bucket::new(rate, 1000);
        assert!(bucket.take(rate, 200, 1000));
        assert!(!bucket.take(rate, 1, 1000));
        assert!(bucket.take(rate, 50, 1500));
        assert!(!bucket.take(rate, 1, 1200));
        assert!(!bucket.take(rate, 1, 1500)); // Rolling the clock back did not mint credit.
        assert!(bucket.take(rate, 200, u64::MAX));
        assert!(!bucket.take(rate, 1, u64::MAX));
        let mut bucket = Bucket::new(rate, 0);
        assert!(!bucket.take(rate, u32::MAX, 0));
        for _ in 0..4 {
            assert!(bucket.take(rate, 0, 0));
        }
        assert!(!bucket.take(rate, 0, 0));
        assert!(bucket.take(rate, 0, 500));
    }

    #[test]
    fn full_table_does_not_evict_live_peers_and_expires_idle_entries() {
        let mut gate = Gate::new(POLICIES[1]);
        for peer in 0..PEERS as u64 {
            assert!(gate.allow(Peer::Steam(peer), 32, 0));
        }
        assert!(!gate.allow(Peer::Steam(999), 32, 0));
        assert!(gate.allow(Peer::Steam(0), 32, 1));
        assert!(gate.allow(Peer::Steam(999), 32, PEER_IDLE_MS));
        assert!(gate
            .peers
            .iter()
            .flatten()
            .any(|entry| entry.peer == Peer::Steam(0)));
    }

    #[test]
    fn distributed_attempts_cannot_bypass_global_budget() {
        let mut gate = Gate::new(POLICIES[0]);
        for peer in 0..256 {
            assert!(gate.allow(Peer::Steam(peer), 0, 0));
        }
        assert!(!gate.allow(Peer::Steam(0), 0, 0));
        assert!(gate.allow(Peer::Steam(0), 0, 8));
    }

    #[test]
    fn concurrent_invites_share_one_peer_budget() {
        let gate = Mutex::new(Gate::new(POLICIES[0]));
        let accepted = AtomicU64::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let gate = &gate;
                let accepted = &accepted;
                scope.spawn(move || {
                    for _ in 0..16 {
                        if gate.lock().unwrap().allow(Peer::Steam(7), 0, 0) {
                            accepted.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        assert_eq!(accepted.load(Ordering::Relaxed), 16);
    }

    #[test]
    fn eighteen_peers_can_exchange_control_traffic_without_shared_mode_state() {
        // The admission policies do not branch on Campaign/MP/Zombies or reset on a mode change.
        for policy in &POLICIES[1..] {
            let mut gate = Gate::new(*policy);
            for tick in 0..1000 {
                for peer in 1..=18 {
                    assert!(gate.allow(Peer::Steam(peer), 1024, tick * 50));
                }
            }
        }
    }
}
