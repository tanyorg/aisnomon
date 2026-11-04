//! Application state: per-line classification, counters, the per-second
//! reception timeline, the MMSI table and the recent-lines log.

use crate::ais::{self, frag, Chan, LenCheck};
use crate::nmea::{self, CkStatus};
use crate::source::Kind;
use crate::term::st;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Recent lines kept for the NMEA RECEIVED view.
pub const RAW_KEEP: usize = 5000;
/// Per-second buckets kept (24 h).
pub const TIMELINE_KEEP: usize = 86_400;
/// Bound on the MMSI table; the least recently heard entry is evicted.
pub const MAX_MMSI: usize = 20_000;
/// Default "recently heard" window for the MMSI count.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(600);
/// Timeline column widths in seconds, cycled with 't'.
pub const SCALES: [u64; 5] = [1, 10, 60, 600, 3600];

// ------------------------------------------------------------ helpers ---

pub fn unix(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Seconds of day -> "HH:MM:SS.sss" with `decimals` fraction digits.
pub fn fmt_tod(sod: f64, decimals: usize) -> String {
    let scale = 10f64.powi(decimals as i32);
    let t = (sod.rem_euclid(86400.0) * scale).round() as u64 % (86400 * scale as u64);
    let frac = t % scale as u64;
    let s = t / scale as u64;
    let base = format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60);
    if decimals == 0 {
        base
    } else {
        format!("{base}.{frac:0decimals$}")
    }
}

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn fmt_utc(t: SystemTime) -> String {
    let u = unix(t);
    let (y, m, d) = civil((u / 86400.0).floor() as i64);
    format!("{y:04}-{m:02}-{d:02} {}Z", fmt_tod(u, 0))
}

/// "3s", "12m05s", "2h03m", "4d07h".
pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        3600..=86399 => format!("{}h{:02}m", s / 3600, s / 60 % 60),
        _ => format!("{}d{:02}h", s / 86400, s / 3600 % 24),
    }
}

// ----------------------------------------------------------- timeline ---

/// Messages per second, total and per channel ([total, A, B]).
pub struct Timeline {
    start: Instant,
    base: u64,
    buckets: VecDeque<[u32; 3]>,
}

impl Timeline {
    pub fn new(start: Instant) -> Self {
        Timeline {
            start,
            base: 0,
            buckets: VecDeque::new(),
        }
    }

    /// Second index of `now` relative to the start.
    pub fn sec(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.start).as_secs()
    }

    pub fn add(&mut self, now: Instant, chan: Chan) {
        let s = self.sec(now);
        let end = self.base + self.buckets.len() as u64;
        if s < self.base {
            return; // cannot happen with a monotonic clock
        }
        if s >= end + TIMELINE_KEEP as u64 {
            self.buckets.clear();
            self.base = s;
        }
        while self.base + (self.buckets.len() as u64) <= s {
            self.buckets.push_back([0; 3]);
        }
        while self.buckets.len() > TIMELINE_KEEP {
            self.buckets.pop_front();
            self.base += 1;
        }
        let b = self.buckets.back_mut().expect("pushed");
        b[0] += 1;
        match chan {
            Chan::A => b[1] += 1,
            Chan::B => b[2] += 1,
            Chan::Unknown => {}
        }
    }

    pub fn get(&self, sec: u64) -> [u32; 3] {
        sec.checked_sub(self.base)
            .and_then(|i| self.buckets.get(i as usize))
            .copied()
            .unwrap_or([0; 3])
    }

    /// Sum over seconds [from, to).
    pub fn sum(&self, from: u64, to: u64) -> [u64; 3] {
        let mut s = [0u64; 3];
        for sec in from.max(self.base)..to {
            let b = self.get(sec);
            for i in 0..3 {
                s[i] += b[i] as u64;
            }
        }
        s
    }

    /// Mean messages/s over the last `secs` complete seconds (fewer at
    /// start-up), or None before the first second has completed.
    pub fn rate(&self, now: Instant, secs: u64) -> Option<f64> {
        let cur = self.sec(now);
        let n = secs.min(cur);
        if n == 0 {
            return None;
        }
        Some(self.sum(cur - n, cur)[0] as f64 / n as f64)
    }
}

// --------------------------------------------------------------- MMSI ---

#[derive(Debug, Clone)]
pub struct MmsiStat {
    pub first: Instant,
    pub last: Instant,
    pub count: u64,
    /// Messages per channel [A, B, unknown].
    pub chan: [u64; 3],
    /// Bit n set when message type n was heard.
    pub types: u32,
}

impl MmsiStat {
    /// Mean interval between messages, once there are two.
    pub fn mean_interval(&self) -> Option<Duration> {
        (self.count >= 2)
            .then(|| (self.last - self.first) / (self.count - 1).min(u32::MAX as u64) as u32)
    }

    pub fn type_list(&self) -> String {
        (1..=27)
            .filter(|t| self.types & (1 << t) != 0)
            .map(|t: u32| t.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

// ----------------------------------------------------------- raw lines ---

pub struct RawLine {
    pub ts: SystemTime,
    pub chan: char,
    /// "1/2" for fragments, empty otherwise.
    pub frag: String,
    /// Result: "T1 244123456", "CK bad", "frag", ...
    pub note: String,
    pub style: u8,
    pub text: String,
}

// ------------------------------------------------------------ counters ---

#[derive(Debug, Default, Clone)]
pub struct Counters {
    pub lines: u64,
    /// Checksum-valid NMEA sentences.
    pub valid: u64,
    pub ck_bad: u64,
    pub ck_missing: u64,
    pub not_nmea: u64,
    /// Valid sentences that are not VDM/VDO.
    pub other: u64,
    pub tag_blocks: u64,
    pub tag_ck_bad: u64,
    pub vdm_sentences: u64,
    pub vdm_err: u64,
    /// Complete received (VDM) messages with a valid header.
    pub messages: u64,
    pub multipart: u64,
    /// Fragment groups that never completed.
    pub incomplete: u64,
    pub out_of_order: u64,
    pub duplicate: u64,
    pub armor_err: u64,
    pub header_err: u64,
    pub len_short: u64,
    pub len_long: u64,
    /// Own-station (VDO) messages, excluded from reception statistics.
    pub own: u64,
    /// Messages per channel [A, B, unknown].
    pub chan: [u64; 3],
    /// Messages per type (index = type, 1..=27).
    pub types: [u64; 28],
    pub type_last: [Option<Instant>; 28],
}

// ---------------------------------------------------------------- app ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Main,
    Types,
    Mmsi,
    Raw,
}

pub struct App {
    pub target: String,
    pub kind: Kind,
    pub status: String,
    pub connected: bool,
    pub reconnects: u64,
    pub ascii: bool,
    pub started: Instant,
    pub window: Duration,
    pub c: Counters,
    pub timeline: Timeline,
    pub mmsi: HashMap<u32, MmsiStat>,
    pub mmsi_evicted: u64,
    pub last_msg: Option<Instant>,
    pub raw: VecDeque<RawLine>,
    /// Lines in `raw` when the view was paused (the frozen bottom).
    pub paused: Option<usize>,
    /// Lines scrolled back from the bottom while paused.
    pub scroll: usize,
    pub scale: usize,
    pub view: View,
    /// MMSI view sorted by message count instead of last heard.
    pub mmsi_by_count: bool,
    reasm: frag::Reassembler,
    esc: Vec<u8>,
}

impl App {
    pub fn new(target: String, ascii: bool, window: Duration, now: Instant) -> Self {
        App {
            target,
            kind: Kind::Unknown,
            status: String::new(),
            connected: false,
            reconnects: 0,
            ascii,
            started: now,
            window,
            c: Counters::default(),
            timeline: Timeline::new(now),
            mmsi: HashMap::new(),
            mmsi_evicted: 0,
            last_msg: None,
            raw: VecDeque::new(),
            paused: None,
            scroll: 0,
            scale: 0,
            view: View::Main,
            mmsi_by_count: false,
            reasm: frag::Reassembler::default(),
            esc: Vec::new(),
        }
    }

    /// Reset statistics (not the connection state).
    pub fn clear(&mut self, now: Instant) {
        self.started = now;
        self.c = Counters::default();
        self.timeline = Timeline::new(now);
        self.mmsi.clear();
        self.mmsi_evicted = 0;
        self.last_msg = None;
        self.raw.clear();
        self.paused = None;
        self.scroll = 0;
        self.reasm.clear();
    }

    pub fn toggle_pause(&mut self) {
        self.paused = match self.paused {
            Some(_) => None,
            None => Some(self.raw.len()),
        };
        self.scroll = 0;
    }

    /// Scroll the paused view by `d` lines (positive = back in time).
    pub fn scroll_by(&mut self, d: isize) {
        if self.paused.is_none() {
            self.toggle_pause();
        }
        let max = self.paused.unwrap_or(0).saturating_sub(1);
        self.scroll = (self.scroll as isize + d).clamp(0, max as isize) as usize;
    }

    /// Handle one key byte. Arrow and page keys arrive as escape sequences.
    /// Returns false when the user asked to quit.
    pub fn key(&mut self, k: u8, page: usize, now: Instant) -> bool {
        if !self.esc.is_empty() || k == 0x1b {
            self.esc.push(k);
            let seq = self.esc.clone();
            let done = match seq.as_slice() {
                [0x1b] | [0x1b, b'['] | [0x1b, b'[', b'5'] | [0x1b, b'[', b'6'] => false,
                [0x1b, b'[', b'A'] => {
                    self.scroll_by(1);
                    true
                }
                [0x1b, b'[', b'B'] => {
                    self.scroll_by(-1);
                    true
                }
                [0x1b, b'[', b'5', b'~'] => {
                    self.scroll_by(page as isize);
                    true
                }
                [0x1b, b'[', b'6', b'~'] => {
                    self.scroll_by(-(page as isize));
                    true
                }
                _ => true,
            };
            if done {
                self.esc.clear();
            }
            return true;
        }
        match k {
            b'q' | b'Q' | 3 | 4 => return false, // q, ^C, ^D
            b'p' | b'P' | b' ' => self.toggle_pause(),
            b'k' => self.scroll_by(1),
            b'j' => self.scroll_by(-1),
            b'b' => self.scroll_by(page as isize),
            b'f' => self.scroll_by(-(page as isize)),
            b'c' | b'C' => self.clear(now),
            b's' | b'S' => self.mmsi_by_count = !self.mmsi_by_count,
            b't' | b'T' => self.scale = (self.scale + 1) % SCALES.len(),
            b'1' => self.view = View::Main,
            b'2' => self.view = View::Types,
            b'3' => self.view = View::Mmsi,
            b'4' => self.view = View::Raw,
            _ => {}
        }
        true
    }

    /// Count fragment groups that timed out. Called on every event.
    pub fn tick(&mut self, now: Instant) {
        self.c.incomplete += self.reasm.expire(now) as u64;
    }

    pub fn pending_frags(&self) -> usize {
        self.reasm.pending()
    }

    pub fn mmsi_recent(&self, now: Instant) -> usize {
        self.mmsi
            .values()
            .filter(|m| now.saturating_duration_since(m.last) <= self.window)
            .count()
    }

    fn push_raw(&mut self, l: RawLine) {
        self.raw.push_back(l);
        if self.raw.len() > RAW_KEEP {
            self.raw.pop_front();
            if let Some(p) = self.paused.as_mut() {
                // The frozen view keeps showing the same lines.
                *p = p.saturating_sub(1);
                if *p == 0 {
                    self.paused = None;
                    self.scroll = 0;
                }
            }
        }
    }

    fn note_mmsi(&mut self, mmsi: u32, mtype: u8, chan: Chan, now: Instant) {
        if !self.mmsi.contains_key(&mmsi) && self.mmsi.len() >= MAX_MMSI {
            if let Some(old) = self
                .mmsi
                .iter()
                .min_by_key(|(_, m)| m.last)
                .map(|(k, _)| *k)
            {
                self.mmsi.remove(&old);
                self.mmsi_evicted += 1;
            }
        }
        let m = self.mmsi.entry(mmsi).or_insert(MmsiStat {
            first: now,
            last: now,
            count: 0,
            chan: [0; 3],
            types: 0,
        });
        m.last = now;
        m.count += 1;
        m.chan[chan.index()] += 1;
        m.types |= 1 << mtype;
    }

    /// Process one received line.
    pub fn line(&mut self, ts: SystemTime, text: String, now: Instant) {
        self.tick(now);
        self.c.lines += 1;
        let mut raw = RawLine {
            ts,
            chan: ' ',
            frag: String::new(),
            note: String::new(),
            style: 0,
            text: String::new(),
        };
        self.classify(&text, now, &mut raw);
        raw.text = text;
        self.push_raw(raw);
    }

    fn classify(&mut self, text: &str, now: Instant, raw: &mut RawLine) {
        let (tag, rest) = nmea::strip_tag(text);
        if let Some(tb) = &tag {
            self.c.tag_blocks += 1;
            if tb.ck != CkStatus::Ok {
                self.c.tag_ck_bad += 1;
            }
        }
        let Some(s) = nmea::split(rest) else {
            self.c.not_nmea += 1;
            raw.note = "not NMEA".into();
            raw.style = st::YELLOW;
            return;
        };
        match s.ck {
            CkStatus::Ok => {}
            CkStatus::Bad { got, want } => {
                self.c.ck_bad += 1;
                raw.note = format!("CK {got:02X}!={want:02X}");
                raw.style = st::RED;
                return;
            }
            CkStatus::Missing => {
                self.c.ck_missing += 1;
                raw.note = "no CK".into();
                raw.style = st::RED;
                return;
            }
        }
        self.c.valid += 1;
        if tag.as_ref().is_some_and(|t| t.ck != CkStatus::Ok) {
            raw.note = "tag CK".into();
            raw.style = st::YELLOW;
        }
        if !ais::is_vdm(&s) {
            self.c.other += 1;
            raw.note = s.addr.chars().take(10).collect();
            raw.style = st::DIM;
            return;
        }
        self.c.vdm_sentences += 1;
        let v = match ais::parse_vdm(&s) {
            Ok(v) => v,
            Err(e) => {
                self.c.vdm_err += 1;
                raw.note = format!("bad {e}");
                raw.style = st::RED;
                return;
            }
        };
        raw.chan = v.chan.letter();
        if v.count > 1 {
            raw.frag = format!("{}/{}", v.num, v.count);
        }
        let fed = self.reasm.feed(s.addr, &v, now);
        self.c.incomplete += fed.dropped as u64;
        self.c.out_of_order += fed.out_of_order as u64;
        self.c.duplicate += fed.duplicate as u64;
        let (payload, fill, count) = match fed.out {
            frag::Out::Pending { .. } => {
                raw.note = if fed.duplicate {
                    "dup frag".into()
                } else if fed.out_of_order {
                    "frag order".into()
                } else if fed.dropped > 0 {
                    "frag (lost)".into()
                } else {
                    "frag".into()
                };
                raw.style = if fed.duplicate || fed.out_of_order || fed.dropped > 0 {
                    st::YELLOW
                } else {
                    st::DIM
                };
                return;
            }
            frag::Out::Complete {
                payload,
                fill,
                count,
            } => (payload, fill, count),
        };
        let bits = match ais::dearmor(&payload, fill) {
            Ok(b) => b,
            Err(e) => {
                self.c.armor_err += 1;
                raw.note = e.to_string();
                raw.style = st::RED;
                return;
            }
        };
        let h = match ais::header(&bits) {
            Ok(h) => h,
            Err(e) => {
                self.c.header_err += 1;
                raw.note = e.to_string();
                raw.style = st::RED;
                return;
            }
        };
        let mark = if v.own { "own " } else { "" };
        raw.note = format!("{mark}T{} {:09}", h.mtype, h.mmsi);
        raw.style = if v.own { st::CYAN } else { 0 };
        match h.len {
            LenCheck::Ok => {}
            LenCheck::Short => {
                self.c.len_short += 1;
                raw.note.push_str(&format!(" short {}b", h.bits));
                raw.style = st::YELLOW;
            }
            LenCheck::Long => {
                self.c.len_long += 1;
                raw.note.push_str(&format!(" long {}b", h.bits));
                raw.style = st::YELLOW;
            }
        }
        if fed.out_of_order {
            raw.style = st::YELLOW;
        }
        if v.own {
            self.c.own += 1;
            return;
        }
        self.c.messages += 1;
        if count > 1 {
            self.c.multipart += 1;
        }
        self.c.chan[v.chan.index()] += 1;
        self.c.types[h.mtype as usize] += 1;
        self.c.type_last[h.mtype as usize] = Some(now);
        self.last_msg = Some(now);
        self.timeline.add(now, v.chan);
        self.note_mmsi(h.mmsi, h.mtype, v.chan, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (App, Instant) {
        let t = Instant::now();
        (App::new("test".into(), false, DEFAULT_WINDOW, t), t)
    }

    #[test]
    fn formatting() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_761), (2026, 11, 4));
        assert_eq!(fmt_tod(3661.2345, 3), "01:01:01.235");
        assert_eq!(fmt_tod(86399.9999, 3), "00:00:00.000");
        assert_eq!(fmt_tod(59.0, 0), "00:00:59");
        assert_eq!(fmt_dur(Duration::from_secs(59)), "59s");
        assert_eq!(fmt_dur(Duration::from_secs(725)), "12m05s");
        assert_eq!(fmt_dur(Duration::from_secs(7380)), "2h03m");
        assert_eq!(fmt_dur(Duration::from_secs(4 * 86400 + 7 * 3600)), "4d07h");
    }

    #[test]
    fn classification_and_counters() {
        let (mut a, t) = app();
        let ts = SystemTime::now();
        for l in [
            "!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5C",
            "!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5D", // bad CK
            "!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0",    // no CK
            "garbage \u{1}\u{2}",
            "$GPRMC,,V,,,,,,,,,,N*53",
            "!AIVDM,2,1,1,A,55?MbV02;H;s<HtKR20EHE:0@T4@Dn2222222216L961O5Gf0NSQEp6ClRp8,0*1C",
            "!AIVDM,2,2,1,A,88888888880,2*25",
            "!AIVDO,1,1,,,177KQJ5000G?tO`K>RA1wUbN0TKH,0*1C",
        ] {
            a.line(ts, l.into(), t);
        }
        let c = &a.c;
        assert_eq!(c.lines, 8);
        assert_eq!((c.valid, c.ck_bad, c.ck_missing, c.not_nmea), (5, 1, 1, 1));
        assert_eq!(
            (c.other, c.vdm_sentences, c.messages, c.multipart),
            (1, 4, 2, 1)
        );
        assert_eq!(c.own, 1);
        assert_eq!(c.chan, [1, 1, 0]);
        assert_eq!((c.types[1], c.types[5]), (1, 1));
        assert_eq!(a.mmsi.len(), 2);
        assert_eq!(a.mmsi_recent(t), 2);
        let notes: Vec<&str> = a.raw.iter().map(|r| r.note.as_str()).collect();
        assert_eq!(
            notes,
            [
                "T1 477553000",
                "CK 5D!=5C",
                "no CK",
                "not NMEA",
                "GPRMC",
                "frag",
                "T5 351759000",
                "own T1 477553000"
            ]
        );
        assert_eq!(a.raw[5].frag, "1/2");
        assert_eq!(a.raw[0].chan, 'B');
        // MMSIs keep their leading zeros: 00MIDxxxx is a coast station
        a.line(ts, "!AIVDM,1,1,,B,D0476ch0@Bfp?`B`?`0,4*5D".into(), t);
        assert_eq!(a.raw.back().unwrap().note, "T20 004310703");
    }

    #[test]
    fn incomplete_groups_counted() {
        let (mut a, t) = app();
        let ts = SystemTime::now();
        a.line(
            ts,
            "!AIVDM,2,1,1,A,55?MbV02;H;s<HtKR20EHE:0@T4@Dn2222222216L961O5Gf0NSQEp6ClRp8,0*1C"
                .into(),
            t,
        );
        a.tick(t + frag::TIMEOUT);
        assert_eq!(a.c.incomplete, 1);
        assert_eq!(a.c.messages, 0);
    }

    #[test]
    fn timeline_rates_and_gaps() {
        let t = Instant::now();
        let mut tl = Timeline::new(t);
        assert_eq!(tl.rate(t, 10), None);
        for i in 0..10 {
            for _ in 0..i {
                tl.add(t + Duration::from_secs(i), Chan::A);
            }
            tl.add(t + Duration::from_secs(i), Chan::B);
        }
        assert_eq!(tl.get(3), [4, 3, 1]);
        assert_eq!(tl.sum(0, 10), [55, 45, 10]);
        let r = tl.rate(t + Duration::from_secs(10), 10).unwrap();
        assert!((r - 5.5).abs() < 1e-9);
        // a gap longer than the buffer resets it
        let far = t + Duration::from_secs(10 + TIMELINE_KEEP as u64 * 2);
        tl.add(far, Chan::Unknown);
        assert_eq!(tl.get(tl.sec(far)), [1, 0, 0]);
        assert_eq!(tl.get(3), [0; 3]);
        assert!(tl.buckets.len() <= TIMELINE_KEEP);
    }

    #[test]
    fn pause_scroll_and_keys() {
        let (mut a, t) = app();
        for i in 0..10 {
            a.line(SystemTime::now(), format!("x{i}"), t);
        }
        assert!(a.key(b'p', 5, t));
        assert_eq!(a.paused, Some(10));
        a.line(SystemTime::now(), "late".into(), t);
        assert_eq!(a.paused, Some(10));
        for k in *b"\x1b[5~" {
            a.key(k, 5, t);
        }
        assert_eq!(a.scroll, 5);
        for k in *b"\x1b[A" {
            a.key(k, 5, t);
        }
        assert_eq!(a.scroll, 6);
        a.key(b'b', 50, t);
        assert_eq!(a.scroll, 9);
        a.key(b' ', 5, t);
        assert_eq!((a.paused, a.scroll), (None, 0));
        a.key(b't', 5, t);
        assert_eq!(a.scale, 1);
        a.key(b'3', 5, t);
        assert_eq!(a.view, View::Mmsi);
        assert!(!a.key(b'q', 5, t));
    }

    #[test]
    fn mmsi_table_bounded() {
        let (mut a, t) = app();
        for i in 0..(MAX_MMSI as u32 + 10) {
            a.note_mmsi(
                200_000_000 + i,
                1,
                Chan::A,
                t + Duration::from_millis(i as u64),
            );
        }
        assert_eq!(a.mmsi.len(), MAX_MMSI);
        assert_eq!(a.mmsi_evicted, 10);
        assert!(!a.mmsi.contains_key(&200_000_000));
    }

    /// Mutated and random input must never panic, and every line is counted
    /// exactly once in one of the top-level classes.
    #[test]
    fn garbage_never_panics() {
        let (mut a, t) = app();
        let seeds = [
            "!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5C",
            "!AIVDM,2,1,1,A,55?MbV02;H;s<HtKR20EHE:0@T4@Dn2222222216L961O5Gf0NSQEp6ClRp8,0*1C",
            "!AIVDM,2,2,1,A,88888888880,2*25",
            "\\s:x,c:1*00\\!AIVDM,1,1,,A,1,0*00",
        ];
        let alphabet: &[u8] = b"!$*,\\0123456789ABW`wxX:\x7f\xc3\xa9";
        let mut x: u64 = 0x2545_f491_4f6c_dd1d;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let ts = SystemTime::now();
        let n = 20_000;
        for i in 0..n {
            let mut b = seeds[i % seeds.len()].as_bytes().to_vec();
            for _ in 0..(rnd() % 4) {
                let pos = (rnd() as usize) % (b.len() + 1);
                let c = alphabet[(rnd() as usize) % alphabet.len()];
                match rnd() % 3 {
                    0 if pos < b.len() => b[pos] = c,
                    1 if pos < b.len() => {
                        b.remove(pos);
                    }
                    _ => b.insert(pos, c),
                }
            }
            let line = String::from_utf8_lossy(&b).into_owned();
            a.line(ts, line, t + Duration::from_millis(i as u64));
            // every few lines, also re-checksum a mutated line so it gets past
            // the checksum and into the VDM/armor/header code
            if let Some(star) = b.iter().rposition(|&c| c == b'*') {
                if b.first() == Some(&b'!') && b.is_ascii() {
                    let body = &b[1..star];
                    let ck = body.iter().fold(0u8, |a, c| a ^ c);
                    let fixed = format!("!{}*{ck:02X}", String::from_utf8_lossy(body));
                    a.line(ts, fixed, t + Duration::from_millis(i as u64));
                }
            }
        }
        let c = &a.c;
        assert_eq!(c.lines, c.valid + c.ck_bad + c.ck_missing + c.not_nmea);
        assert!(c.messages > 0 && c.ck_bad > 0 && c.vdm_err + c.armor_err + c.header_err > 0);
        assert!(a.raw.len() <= RAW_KEEP);
        assert!(a.pending_frags() <= frag::MAX_PENDING);
        crate::ui::render(&a, t + Duration::from_secs(30), ts, 120, 40);
    }
}
