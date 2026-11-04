//! Reassembly of multi-sentence AIS messages.
//!
//! Fragments of one message are keyed by (address, sequential message id,
//! channel). Receivers emit them back to back, so anything that breaks the
//! sequence is reported: a group abandoned because a new one started under
//! the same key, a fragment number seen twice, fragments out of order, and
//! groups that never completed within `TIMEOUT`.

use super::{Chan, Vdm};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A group older than this is considered lost.
pub const TIMEOUT: Duration = Duration::from_secs(3);
/// Bound on concurrently pending groups (seq ids 0..9 x channels x talkers).
pub const MAX_PENDING: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    addr: String,
    seq: Option<u8>,
    chan: Chan,
}

struct Group {
    count: u8,
    parts: Vec<Option<String>>,
    /// Fill bits of the final fragment.
    fill: u8,
    next: u8,
    first: Instant,
}

/// Outcome of feeding one fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// Stored; `have` of `count` fragments present.
    Pending { have: u8, count: u8 },
    /// All fragments present: the joined payload and its fill bits.
    Complete {
        payload: String,
        fill: u8,
        count: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fed {
    pub out: Out,
    /// Earlier incomplete groups discarded because of this fragment.
    pub dropped: u32,
    /// This fragment did not follow its predecessor (including a group
    /// that did not start with fragment 1).
    pub out_of_order: bool,
    /// This fragment number had already been received for the group.
    pub duplicate: bool,
}

#[derive(Default)]
pub struct Reassembler {
    pending: HashMap<Key, Group>,
}

impl Reassembler {
    pub fn clear(&mut self) {
        self.pending.clear();
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn feed(&mut self, addr: &str, v: &Vdm, now: Instant) -> Fed {
        let mut fed = Fed {
            out: Out::Pending {
                have: 1,
                count: v.count,
            },
            dropped: 0,
            out_of_order: false,
            duplicate: false,
        };
        if v.count == 1 {
            fed.out = Out::Complete {
                payload: v.payload.to_string(),
                fill: v.fill,
                count: 1,
            };
            return fed;
        }
        let key = Key {
            addr: addr.to_string(),
            seq: v.seq,
            chan: v.chan,
        };
        // A conflicting fragment means the previous group is lost.
        if let Some(g) = self.pending.get(&key) {
            let conflict = g.count != v.count || g.parts[v.num as usize - 1].is_some();
            if conflict {
                fed.duplicate = g.count == v.count && v.num != 1;
                self.pending.remove(&key);
                fed.dropped += 1;
            }
        }
        if !self.pending.contains_key(&key) && self.pending.len() >= MAX_PENDING {
            if let Some(k) = self
                .pending
                .iter()
                .min_by_key(|(_, g)| g.first)
                .map(|(k, _)| k.clone())
            {
                self.pending.remove(&k);
                fed.dropped += 1;
            }
        }
        let g = self.pending.entry(key.clone()).or_insert_with(|| Group {
            count: v.count,
            parts: vec![None; v.count as usize],
            fill: 0,
            next: 1,
            first: now,
        });
        if v.num != g.next {
            fed.out_of_order = true;
        }
        g.next = v.num + 1;
        g.parts[v.num as usize - 1] = Some(v.payload.to_string());
        if v.num == v.count {
            g.fill = v.fill;
        }
        let have = g.parts.iter().filter(|p| p.is_some()).count() as u8;
        if have < g.count {
            fed.out = Out::Pending {
                have,
                count: g.count,
            };
            return fed;
        }
        let g = self.pending.remove(&key).expect("present");
        fed.out = Out::Complete {
            payload: g.parts.into_iter().flatten().collect(),
            fill: g.fill,
            count: g.count,
        };
        fed
    }

    /// Discard groups older than `TIMEOUT`; returns how many.
    pub fn expire(&mut self, now: Instant) -> u32 {
        let before = self.pending.len();
        self.pending
            .retain(|_, g| now.saturating_duration_since(g.first) < TIMEOUT);
        (before - self.pending.len()) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ais::{dearmor, header, parse_vdm};
    use crate::nmea::split;

    fn feed(r: &mut Reassembler, l: &str, now: Instant) -> Fed {
        let s = split(l).unwrap();
        r.feed(s.addr, &parse_vdm(&s).unwrap(), now)
    }

    const P1: &str =
        "!AIVDM,2,1,1,A,55?MbV02;H;s<HtKR20EHE:0@T4@Dn2222222216L961O5Gf0NSQEp6ClRp8,0*1C";
    const P2: &str = "!AIVDM,2,2,1,A,88888888880,2*25";

    #[test]
    fn two_part_static_report() {
        let mut r = Reassembler::default();
        let t = Instant::now();
        let f = feed(&mut r, P1, t);
        assert_eq!(f.out, Out::Pending { have: 1, count: 2 });
        assert!(!f.out_of_order);
        let f = feed(&mut r, P2, t);
        let Out::Complete {
            payload,
            fill,
            count,
        } = f.out
        else {
            panic!("{f:?}")
        };
        assert_eq!((fill, count, f.dropped), (2, 2, 0));
        let h = header(&dearmor(&payload, fill).unwrap()).unwrap();
        assert_eq!((h.mtype, h.mmsi, h.bits), (5, 351759000, 424));
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn lost_and_reordered_fragments() {
        let t = Instant::now();
        // second fragment alone: out of order, then times out
        let mut r = Reassembler::default();
        let f = feed(&mut r, P2, t);
        assert!(f.out_of_order);
        assert_eq!(r.expire(t + Duration::from_secs(1)), 0);
        assert_eq!(r.expire(t + TIMEOUT), 1);
        // reversed order still completes, flagged
        let f = feed(&mut r, P2, t);
        assert!(f.out_of_order);
        let f = feed(&mut r, P1, t);
        assert!(matches!(f.out, Out::Complete { .. }) && f.out_of_order);
        // first fragment repeated: previous group dropped
        feed(&mut r, P1, t);
        let f = feed(&mut r, P1, t);
        assert_eq!((f.dropped, f.duplicate), (1, false));
        assert_eq!(f.out, Out::Pending { have: 1, count: 2 });
        // second fragment repeated is a duplicate
        let f = feed(&mut r, P2, t);
        assert!(matches!(f.out, Out::Complete { .. }));
        feed(&mut r, P1, t);
        feed(&mut r, P2, t);
        let f = feed(&mut r, P2, t);
        assert_eq!((f.dropped, f.duplicate), (0, false)); // new group: 2 alone
        let f = feed(&mut r, "!AIVDM,2,2,1,A,88888888880,2*25", t);
        assert_eq!((f.dropped, f.duplicate), (1, true));
    }

    #[test]
    fn keys_separate_channels_and_bounded() {
        let mut r = Reassembler::default();
        let t = Instant::now();
        feed(&mut r, P1, t);
        let f = feed(&mut r, "!AIVDM,2,2,1,B,88888888880,2*26", t);
        assert!(matches!(f.out, Out::Pending { .. }));
        assert_eq!(r.pending(), 2);
        let mut r = Reassembler::default();
        let mut dropped = 0;
        for i in 0..200u32 {
            let s = format!("!AIVDM,2,1,{},A,1,0", i % 10);
            let s = split(&s).unwrap();
            let addr = format!("A{}", i / 10);
            dropped += r.feed(&addr, &parse_vdm(&s).unwrap(), t).dropped;
        }
        assert_eq!(r.pending(), MAX_PENDING);
        assert_eq!(dropped, 200 - MAX_PENDING as u32);
    }
}
