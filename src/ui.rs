//! Screen layout. Main view: SUMMARY, RECEPTION TIMELINE, NMEA RECEIVED.
//! Detail views: channels/types/errors, MMSI table, full-height raw log.

use crate::ais;
use crate::app::{fmt_dur, fmt_tod, fmt_utc, unix, App, RawLine, Stats, View, SCALES};
use crate::term::{st, Canvas};
use std::time::{Duration, Instant, SystemTime};

pub const MIN_W: usize = 80;
pub const MIN_H: usize = 24;
const SUMMARY_H: usize = 7;
/// A message older than this makes "Last msg" yellow.
const QUIET: Duration = Duration::from_secs(60);

extern "C" {
    fn gethostname(name: *mut std::ffi::c_char, len: usize) -> i32;
}

/// Short host name, so a screen shared over SSH says where it runs.
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most len bytes into buf.
    let r = unsafe { gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if r != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let h = String::from_utf8_lossy(&buf[..end]).into_owned();
    h.split('.').next().unwrap_or("").to_string()
}

/// The first `n` characters of `s`, ending in "..." when cut.
fn head(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut t: String = s.chars().take(n.saturating_sub(3)).collect();
    t.push_str(&"..."[..n.min(3)]);
    t
}

/// The last `n` characters of `s`, with a leading "..." when cut.
fn tail(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len <= n {
        return s.to_string();
    }
    let keep = n.saturating_sub(3);
    format!("...{}", s.chars().skip(len - keep).collect::<String>())
}

/// `n` in at most `w` (>= 4) characters: plain digits when they fit,
/// else scaled with decimal k/M/G/T/P/E, e.g. 12.3M.
pub fn fmt_count(n: u64, w: usize) -> String {
    let plain = n.to_string();
    if plain.len() <= w {
        return plain;
    }
    let units = [
        (1e3, 'k'),
        (1e6, 'M'),
        (1e9, 'G'),
        (1e12, 'T'),
        (1e15, 'P'),
        (1e18, 'E'),
    ];
    for (div, unit) in units {
        let v = n as f64 / div;
        // Use the first unit that keeps the value below 1000 once rounded.
        if v >= 999.95 {
            continue;
        }
        let s = format!("{v:.1}{unit}");
        if s.len() <= w {
            return s;
        }
        if v < 999.5 {
            let s = format!("{v:.0}{unit}");
            if s.len() <= w {
                return s;
            }
        }
    }
    plain
}

/// Age with one unit, for narrow columns: 13s, 5m, 2h, 30d.
fn fmt_ago(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", (s / 86400).min(999)),
    }
}

/// Put at most `w` characters, so text never overwrites a box border.
fn put_clip(c: &mut Canvas, x: usize, y: usize, s: &str, style: u8, w: usize) {
    let t: String = s.chars().take(w).collect();
    c.put(x, y, &t, style);
}

fn pct(n: u64, d: u64) -> String {
    if d == 0 {
        "-".into()
    } else {
        format!("{:.1}%", n as f64 * 100.0 / d as f64)
    }
}

/// Exact compact age for axis labels: 30s, 1m30s, 5m, 2h, 1h30m, 1d.
fn fmt_age(s: u64) -> String {
    let (d, h, m, sec) = (s / 86400, s / 3600 % 24, s / 60 % 60, s % 60);
    let mut out = String::new();
    for (v, u) in [(d, "d"), (h, "h"), (m, "m"), (sec, "s")] {
        if v > 0 {
            out.push_str(&format!("{v}{u}"));
        }
    }
    if out.is_empty() {
        out.push_str("0s");
    }
    out
}

/// Tick spacing in seconds: the smallest "round" interval that leaves at
/// least `min_cols` columns between labels.
fn tick_interval(scale: u64, min_cols: u64) -> u64 {
    const NICE: [u64; 17] = [
        5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10800, 21600, 43200, 86400, 172800,
    ];
    NICE.iter()
        .copied()
        .find(|&t| t % scale == 0 && t / scale >= min_cols)
        .unwrap_or(scale * min_cols)
}

fn fmt_rate(r: Option<f64>) -> String {
    match r {
        None => "-".into(),
        Some(r) if r < 10.0 => format!("{r:.2}/s"),
        Some(r) => format!("{r:.1}/s"),
    }
}

pub fn window_label(d: Duration) -> String {
    let s = d.as_secs();
    if s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

pub fn render(app: &App, now: Instant, wall: SystemTime, w: usize, h: usize) -> Canvas {
    let mut c = Canvas::new(w, h, app.ascii);
    if w < MIN_W || h < MIN_H {
        c.put(
            0,
            0,
            &format!("terminal too small: {w}x{h}, need {MIN_W}x{MIN_H}"),
            st::BOLD,
        );
        c.put(0, 1, "q quits", 0);
        return c;
    }
    header(&mut c, app, wall);
    // While paused everything below the header is the frozen picture,
    // described at the instant it was taken.
    let (s, now) = app.shown(now);
    let body = h - 2;
    match app.view {
        View::Main => {
            summary(&mut c, s, now, 1, w);
            let rest = body - SUMMARY_H;
            let tl = (rest * 2 / 5).clamp(6, 14);
            timeline(&mut c, app, s, now, 1 + SUMMARY_H, w, tl);
            raw_box(&mut c, app, s, 1 + SUMMARY_H + tl, w, rest - tl);
        }
        View::Types => types_view(&mut c, s, now, 1, w, body),
        View::Mmsi => mmsi_view(&mut c, app, s, now, 1, w, body),
        View::Raw => raw_box(&mut c, app, s, 1, w, body),
    }
    footer(&mut c, app, h - 1);
    c
}

/// Rows of the scrollable list in the current view (raw log or MMSI
/// table), for page-wise scrolling.
pub fn page_rows(app: &App, h: usize) -> usize {
    if h < MIN_H {
        return 1;
    }
    match app.view {
        View::Raw => h - 4,
        View::Mmsi => h - 5,
        _ => {
            let rest = h - 2 - SUMMARY_H;
            (rest - (rest * 2 / 5).clamp(6, 14))
                .saturating_sub(2)
                .max(1)
        }
    }
}

fn header(c: &mut Canvas, app: &App, wall: SystemTime) {
    let host = hostname();
    let mut x = c.put(
        0,
        0,
        &format!("aisnomon {}", env!("CARGO_PKG_VERSION")),
        st::BOLD,
    );
    if !host.is_empty() {
        x = c.put(x, 0, &format!("  {host}"), st::CYAN);
    }
    x = c.put(x, 0, &format!("  {}", tail(&app.target, c.w / 3)), 0);
    let clock = fmt_utc(wall);
    // two spaces before the status, at least one before the clock
    let room = c.w.saturating_sub(x + 2 + clock.len() + 1);
    let status = head(&app.status, room);
    let style = if app.connected { st::GREEN } else { st::YELLOW };
    c.put(x + 2, 0, &status, style);
    c.put(c.w - clock.len(), 0, &clock, 0);
}

fn footer(c: &mut Canvas, app: &App, y: usize) {
    let mut x = 0;
    for (k, label, v) in [
        ("1", "main", View::Main),
        ("2", "types", View::Types),
        ("3", "MMSI", View::Mmsi),
        ("4", "raw", View::Raw),
    ] {
        let style = if app.view == v { st::REV } else { 0 };
        x = c.put(x, y, &format!("{k} {label}"), style);
        x = c.put(x, y, " ", 0);
    }
    if let Some(f) = &app.frozen {
        let tod = fmt_tod(unix(f.wall), 0);
        x = c.put(x, y, &format!(" PAUSED {tod}Z "), st::REV | st::YELLOW);
        let keys = match app.view {
            View::Types => " p resume  c clear  q quit",
            View::Mmsi => " p resume  j/k scroll  s sort  q quit",
            _ => " p resume  j/k scroll  t scale  q quit",
        };
        c.put(x, y, keys, st::DIM);
        return;
    }
    let keys = match app.view {
        View::Types => " p pause  c clear  q quit",
        View::Mmsi => " p pause  j/k scroll  s sort  c clear  q quit",
        _ => " p pause  j/k scroll  t scale  c clear  q quit",
    };
    c.put(x, y, keys, st::DIM);
}

/// Label width per SUMMARY column: the longest label plus one space. The
/// middle column is narrower so that "count percent" never touches the
/// third column's labels at 80 columns.
const LABEL_W: [usize; 3] = [11, 9, 11];

fn field(c: &mut Canvas, col: (usize, usize), y: usize, label: &str, value: &str, style: u8) {
    let (x, i) = col;
    c.put(x, y, label, st::DIM);
    c.put(x + LABEL_W[i], y, value, style);
}

fn summary(c: &mut Canvas, s: &Stats, now: Instant, y: usize, w: usize) {
    c.boxed(0, y, w, SUMMARY_H, "SUMMARY");
    let k = &s.c;
    let cw = (w - 4) / 3;
    let (x1, x2, x3) = ((2, 0), (2 + cw, 1), (2 + 2 * cw, 2));
    let y = y + 1;

    field(c, x1, y, "Messages", &k.messages.to_string(), st::BOLD);
    field(
        c,
        x1,
        y + 1,
        "Rate 10s",
        &fmt_rate(s.timeline.rate(now, 10)),
        0,
    );
    field(
        c,
        x1,
        y + 2,
        "Rate 1m",
        &fmt_rate(s.timeline.rate(now, 60)),
        0,
    );
    let (last, style) = match s.last_msg {
        None => ("-".to_string(), st::DIM),
        Some(t) => {
            let d = now.saturating_duration_since(t);
            let s = if d >= QUIET { st::YELLOW } else { 0 };
            (format!("{} ago", fmt_dur(d)), s)
        }
    };
    field(c, x1, y + 3, "Last msg", &last, style);
    field(
        c,
        x1,
        y + 4,
        "Uptime",
        &fmt_dur(now.saturating_duration_since(s.started)),
        0,
    );

    let total = k.messages;
    let chan = |n: u64| format!("{:<7} {}", fmt_count(n, 7), pct(n, total));
    field(c, x2, y, "AIS A", &chan(k.chan[0]), 0);
    field(c, x2, y + 1, "AIS B", &chan(k.chan[1]), 0);
    field(
        c,
        x2,
        y + 2,
        "No chan",
        &chan(k.chan[2]),
        if k.chan[2] > 0 { st::DIM } else { 0 },
    );
    field(
        c,
        x2,
        y + 3,
        &format!("MMSI {}", window_label(s.window)),
        &s.mmsi_recent(now).to_string(),
        st::BOLD,
    );
    let all = if s.mmsi_evicted > 0 {
        format!("{}+", s.mmsi.len() as u64 + s.mmsi_evicted)
    } else {
        s.mmsi.len().to_string()
    };
    field(c, x2, y + 4, "MMSI all", &all, 0);

    let snt = k.valid + k.ck_bad + k.ck_missing;
    let bad = |n: u64, s: u8| if n > 0 { s } else { 0 };
    field(
        c,
        x3,
        y,
        "Valid",
        &format!("{:<7} {}", fmt_count(k.valid, 7), pct(k.valid, snt)),
        0,
    );
    field(
        c,
        x3,
        y + 1,
        "CK error",
        &format!(
            "{:<7} {}",
            fmt_count(k.ck_bad + k.ck_missing, 7),
            pct(k.ck_bad + k.ck_missing, snt)
        ),
        bad(k.ck_bad + k.ck_missing, st::RED),
    );
    field(
        c,
        x3,
        y + 2,
        "Not NMEA",
        &k.not_nmea.to_string(),
        bad(k.not_nmea, st::YELLOW),
    );
    field(
        c,
        x3,
        y + 3,
        "Incomplete",
        &k.incomplete.to_string(),
        bad(k.incomplete, st::YELLOW),
    );
    let dec = k.vdm_err + k.armor_err + k.header_err + k.len_short + k.len_long;
    field(
        c,
        x3,
        y + 4,
        "Decode err",
        &dec.to_string(),
        bad(dec, st::RED),
    );
}

/// Mean msg/s per column, oldest first. The newest column holds the
/// complete seconds of the current, possibly partial, interval.
pub fn columns(s: &Stats, now: Instant, scale: u64, n: usize) -> Vec<Option<f64>> {
    let cur = s.timeline.sec(now);
    let mut out = vec![None; n];
    if cur == 0 {
        return out;
    }
    let last_start = (cur - 1) / scale * scale;
    for (j, slot) in out.iter_mut().rev().enumerate() {
        let Some(start) = last_start.checked_sub(j as u64 * scale) else {
            break;
        };
        let end = (start + scale).min(cur);
        let sum = s.timeline.sum(start, end)[0];
        *slot = Some(sum as f64 / (end - start) as f64);
    }
    out
}

const BARS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];

fn timeline(c: &mut Canvas, app: &App, s: &Stats, now: Instant, y: usize, w: usize, h: usize) {
    let scale = SCALES[app.scale];
    let title = format!(
        "RECEPTION TIMELINE  msg/s, {}/column  [t]",
        window_label(Duration::from_secs(scale))
    );
    c.boxed(0, y, w, h, &title);
    const LABEL: usize = 8;
    let rows = h - 3; // chart rows; one row for the axis
    let x0 = 1 + LABEL;
    let ncol = w - 2 - LABEL - 1;
    let cols = columns(s, now, scale, ncol);
    let max = cols.iter().flatten().fold(0.0f64, |a, &b| a.max(b));
    let top = if max > 0.0 { max } else { 1.0 };
    c.put(1, y + 1, &format!("{:>7}", fmt_rate(Some(top))), st::DIM);
    c.put(1, y + rows, &format!("{:>7}", "0"), st::DIM);
    for (i, v) in cols.iter().enumerate() {
        let Some(v) = *v else { continue };
        let mut eighths = (v / top * (rows * 8) as f64).round() as usize;
        if v > 0.0 {
            eighths = eighths.max(1);
        }
        for r in 0..rows {
            let fill = eighths.saturating_sub(r * 8).min(8);
            if fill == 0 {
                break;
            }
            let ch = if c.ascii {
                if fill >= 4 {
                    '#'
                } else {
                    '.'
                }
            } else {
                BARS[fill - 1]
            };
            c.put(x0 + i, y + rows - r, &ch.to_string(), st::GREEN);
        }
    }
    // Axis: age labels right-aligned under their column.
    let ay = y + rows + 1;
    c.put(x0 + ncol - 3, ay, "now", st::DIM);
    let mut right_limit = x0 + ncol - 4;
    let step = (tick_interval(scale, 10) / scale) as usize;
    let mut j = step;
    while j < ncol {
        let label = format!("-{}", fmt_age(j as u64 * scale));
        let end = x0 + ncol - j;
        if end + 1 < right_limit && end + 1 >= x0 + label.len() {
            let x = end + 1 - label.len();
            c.put(x, ay, &label, st::DIM);
            right_limit = x.saturating_sub(1);
        }
        j += step;
    }
}

fn raw_line(c: &mut Canvas, l: &RawLine, x: usize, y: usize, w: usize) {
    let tod = fmt_tod(unix(l.ts), 3);
    let mut cx = c.put(x, y, &tod, st::DIM);
    cx = c.put(cx + 1, y, &l.chan.to_string(), 0);
    c.put(cx + 1, y, &l.frag, st::DIM);
    let note: String = l.note.chars().take(20).collect();
    c.put(cx + 5, y, &note, l.style);
    let tx = cx + 26;
    if tx < x + w {
        let text: String = l.text.chars().take(x + w - tx).collect();
        c.put(tx, y, &text, if l.style == st::DIM { st::DIM } else { 0 });
    }
}

fn raw_box(c: &mut Canvas, app: &App, s: &Stats, y: usize, w: usize, h: usize) {
    let title = if !app.is_paused() {
        "NMEA RECEIVED  [p] pause".to_string()
    } else if app.scroll > 0 {
        format!(
            "NMEA RECEIVED  PAUSED, {} lines back  [p] resume  [j/k] scroll",
            app.scroll
        )
    } else {
        "NMEA RECEIVED  PAUSED  [p] resume  [j/k] scroll".to_string()
    };
    c.boxed(0, y, w, h, &title);
    if h < 3 {
        return;
    }
    let n = h - 2;
    let end = s.raw.len().saturating_sub(app.scroll);
    let start = end.saturating_sub(n);
    for (i, l) in s.raw.range(start..end).enumerate() {
        raw_line(c, l, 2, y + 1 + i, w - 4);
    }
}

fn types_view(c: &mut Canvas, s: &Stats, now: Instant, y: usize, w: usize, h: usize) {
    let k = &s.c;
    let lw = 40.min(w / 2);
    c.boxed(0, y, lw, h, "CHANNELS & INTEGRITY");
    let cur = s.timeline.sec(now);
    let span = cur.min(600);
    let recent = s.timeline.sum(cur - span, cur);
    let mut yy = y + 1;
    c.put(2, yy, "Chan     Msgs      %   10m rate", st::DIM);
    yy += 1;
    for (name, i) in [("A", 0), ("B", 1), ("-", 2)] {
        let rate = if i < 2 && span > 0 {
            fmt_rate(Some(recent[i + 1] as f64 / span as f64))
        } else {
            String::new()
        };
        c.put(
            2,
            yy,
            &format!(
                "{name:<4} {:>8} {:>6} {:>10}",
                fmt_count(k.chan[i], 8),
                pct(k.chan[i], k.messages),
                rate
            ),
            0,
        );
        yy += 1;
    }
    yy += 1;
    let rows: [(&str, u64, u8); 20] = [
        ("Lines", k.lines, 0),
        ("Valid sentences", k.valid, 0),
        ("Checksum bad", k.ck_bad, st::RED),
        ("Checksum missing", k.ck_missing, st::RED),
        ("Not NMEA", k.not_nmea, st::YELLOW),
        ("Other sentences", k.other, 0),
        ("Tag blocks", k.tag_blocks, 0),
        ("Tag block CK bad", k.tag_ck_bad, st::YELLOW),
        ("VDM/VDO sentences", k.vdm_sentences, 0),
        ("Bad VDM fields", k.vdm_err, st::RED),
        ("Multi-part msgs", k.multipart, 0),
        ("Incomplete groups", k.incomplete, st::YELLOW),
        ("Fragment order", k.out_of_order, st::YELLOW),
        ("Duplicate frags", k.duplicate, st::YELLOW),
        ("Armor errors", k.armor_err, st::RED),
        ("Header errors", k.header_err, st::RED),
        ("Length short", k.len_short, st::YELLOW),
        ("Length long", k.len_long, st::YELLOW),
        ("Own (VDO) msgs", k.own, st::CYAN),
        ("Pending frags", s.pending as u64, 0),
    ];
    for (label, v, s) in rows {
        if yy >= y + h - 1 {
            break;
        }
        c.put(2, yy, label, st::DIM);
        c.put(
            22,
            yy,
            &format!("{:>10}", fmt_count(v, 10)),
            if v > 0 { s } else { 0 },
        );
        yy += 1;
    }

    let rw = w - lw;
    c.boxed(lw, y, rw, h, "MESSAGE TYPES");
    // Inner width at 80 columns is 36: 2+1+9+7+1+6+5+1+4. Wider
    // terminals also get the length of the latest out-of-range message.
    let iw = rw.saturating_sub(4);
    let wide = iw >= 44;
    let x = lw + 2;
    let head = format!(
        "{:>2} {:<9}{:>7} {:>6}{:>5} {:>4}{}",
        "Ty",
        "Name",
        "Msgs",
        "%",
        "Len!",
        "Last",
        if wide { " LastLen" } else { "" }
    );
    put_clip(c, x, y + 1, &head, st::DIM, iw);
    let mut yy = y + 2;
    for t in 1..=27u8 {
        let n = k.types[t as usize];
        if n == 0 {
            continue;
        }
        if yy >= y + h - 1 {
            break;
        }
        let ti = t as usize;
        let last = k.type_last[ti]
            .map(|l| fmt_ago(now.saturating_duration_since(l)))
            .unwrap_or_default();
        let left = format!(
            "{t:>2} {:<9}{:>7} {:>6}",
            ais::type_name(t),
            fmt_count(n, 7),
            pct(n, k.messages)
        );
        put_clip(c, x, yy, &left, 0, iw);
        let bad = k.type_len_short[ti] + k.type_len_long[ti];
        let len = if bad > 0 {
            fmt_count(bad, 4)
        } else {
            String::new()
        };
        put_clip(c, x + 26, yy, &format!("{len:>5}"), st::YELLOW, iw - 26);
        put_clip(c, x + 31, yy, &format!(" {last:>4}"), 0, iw - 31);
        if wide {
            if let Some(b) = k.type_len_bits[ti] {
                let (lo, hi) = ais::len_range(t);
                let range = if lo == hi {
                    format!("{lo}")
                } else {
                    format!("{lo}-{hi}")
                };
                let note = format!(" {b:>6}b");
                put_clip(c, x + 36, yy, &note, st::YELLOW, iw - 36);
                // expected range, when there is room for it
                put_clip(
                    c,
                    x + 44,
                    yy,
                    &format!(" ({range})"),
                    st::DIM,
                    iw.saturating_sub(44),
                );
            }
        }
        yy += 1;
    }
}

fn mmsi_view(c: &mut Canvas, app: &App, s: &Stats, now: Instant, y: usize, w: usize, h: usize) {
    let sort = if app.mmsi_by_count {
        "by count"
    } else {
        "by last heard"
    };
    c.boxed(
        0,
        y,
        w,
        h,
        &format!(
            "MMSI  {} heard in {}, {} total, sorted {sort}  [s]",
            s.mmsi_recent(now),
            window_label(s.window),
            s.mmsi.len()
        ),
    );
    c.put(
        2,
        y + 1,
        "MMSI       Kind       Msgs      A      B  Interval    Last  Types",
        st::DIM,
    );
    let mut v: Vec<_> = s.mmsi.iter().collect();
    if app.mmsi_by_count {
        v.sort_by(|a, b| b.1.count.cmp(&a.1.count).then(a.0.cmp(b.0)));
    } else {
        v.sort_by(|a, b| b.1.last.cmp(&a.1.last).then(a.0.cmp(b.0)));
    }
    let rows = h.saturating_sub(3);
    // Keep the last page full when scrolled past the end.
    let first = app.mmsi_scroll.min(v.len().saturating_sub(rows));
    for (i, (mmsi, m)) in v.iter().skip(first).take(rows).enumerate() {
        let age = now.saturating_duration_since(m.last);
        let style = if age > s.window { st::DIM } else { 0 };
        let iv = m.mean_interval().map(fmt_dur).unwrap_or_else(|| "-".into());
        let line = format!(
            "{mmsi:09}  {:<8} {:>6} {:>6} {:>6} {:>9} {:>7}  {}",
            ais::mmsi_kind(**mmsi),
            m.count,
            m.chan[0],
            m.chan[1],
            iv,
            fmt_dur(age),
            m.type_list()
        );
        let line: String = line.chars().take(w - 4).collect();
        c.put(2, y + 2 + i, &line, style);
    }
    if v.len() > rows && rows > 0 {
        let pos = format!(
            " {}-{} of {} ",
            first + 1,
            (first + rows).min(v.len()),
            v.len()
        );
        c.put(w - 2 - pos.len(), y + h - 1, &pos, st::DIM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::DEFAULT_WINDOW;

    fn sample_app(t: Instant) -> App {
        let mut a = App::new("udp://0.0.0.0:10110".into(), false, DEFAULT_WINDOW, t);
        let ts = SystemTime::now();
        for i in 0..120u64 {
            let now = t + Duration::from_millis(i * 500);
            a.line(
                ts,
                "!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5C".into(),
                now,
            );
            a.line(
                ts,
                "!AIVDM,2,1,1,A,55?MbV02;H;s<HtKR20EHE:0@T4@Dn2222222216L961O5Gf0NSQEp6ClRp8,0*1C"
                    .into(),
                now,
            );
            a.line(ts, "!AIVDM,2,2,1,A,88888888880,2*25".into(), now);
        }
        a.line(
            ts,
            "!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*00".into(),
            t,
        );
        a
    }

    #[test]
    fn render_sizes_never_panic() {
        let t = Instant::now();
        let mut a = sample_app(t);
        let now = t + Duration::from_secs(61);
        for view in [View::Main, View::Types, View::Mmsi, View::Raw] {
            a.view = view;
            for scale in 0..SCALES.len() {
                a.scale = scale;
                for (w, h) in [
                    (80, 24),
                    (100, 40),
                    (81, 25),
                    (200, 60),
                    (1000, 1000),
                    (79, 24),
                    (10, 3),
                ] {
                    render(&a, now, SystemTime::now(), w, h);
                    page_rows(&a, h);
                }
            }
        }
        // empty app, before the first second
        let e = App::new("x".into(), true, DEFAULT_WINDOW, t);
        render(&e, t, SystemTime::now(), 80, 24);
    }

    #[test]
    fn main_view_content() {
        let t = Instant::now();
        let a = sample_app(t);
        let s = render(&a, t + Duration::from_secs(60), SystemTime::now(), 100, 30).text();
        assert!(s.contains("SUMMARY"), "{s}");
        assert!(s.contains("RECEPTION TIMELINE"));
        assert!(s.contains("NMEA RECEIVED"));
        assert!(s.contains("T5 351759000"));
        assert!(s.contains("CK 00!=5C"));
        assert!(s.contains("Messages   240"), "{s}");
        // 240 messages in 60 s -> 4.00/s over the last minute
        assert!(s.contains("Rate 1m    4.00/s"), "{s}");
        // a long target never runs into the clock
        let mut b = sample_app(t);
        b.target = "x".repeat(300);
        let s = render(&b, t, SystemTime::UNIX_EPOCH, 80, 24).text();
        assert!(
            s.lines().next().unwrap().ends_with("1970-01-01 00:00:00Z"),
            "{s}"
        );
    }

    #[test]
    fn summary_columns_do_not_touch() {
        let t = Instant::now();
        let mut a = sample_app(t);
        a.live.c.messages = 9_999_999;
        a.live.c.chan = [9_999_999, 0, 0];
        a.live.c.valid = 9_999_999;
        let s = render(&a, t, SystemTime::now(), 80, 24).text();
        let cw = (80 - 4) / 3;
        for row in s.lines().skip(2).take(5) {
            let b: Vec<char> = row.chars().collect();
            assert_eq!(b[2 + 2 * cw - 1], ' ', "{row}");
            assert_eq!(b[2 + cw - 1], ' ', "{row}");
        }
        assert!(s.contains("AIS A    9999999 100.0%"), "{s}");
    }

    /// Every box keeps its right border intact at the minimum width, with
    /// the widest values each view can show.
    #[test]
    fn borders_survive_wide_values() {
        let t = Instant::now();
        let mut a = sample_app(t);
        for ty in 1..=27 {
            a.live.c.types[ty] = 99_999_999;
            a.live.c.type_last[ty] = Some(t);
        }
        a.live.c.messages = 99_999_999;
        a.live.c.chan = [99_999_999; 3];
        let now = t + Duration::from_secs(86_400 * 30);
        for view in [View::Main, View::Types, View::Mmsi, View::Raw] {
            a.view = view;
            let s = render(&a, now, SystemTime::now(), 80, 40).text();
            for (i, row) in s.lines().enumerate().skip(1).take(38) {
                let last = row.chars().nth(79);
                assert!(
                    matches!(last, Some('\u{2502}' | '\u{2510}' | '\u{2518}')),
                    "{view:?} row {i}: {row}"
                );
            }
        }
        a.view = View::Types;
        let s = render(&a, now, SystemTime::now(), 80, 40).text();
        assert!(s.contains(" 5 Static A  100.0M 100.0%       30d"), "{s}");
        // length errors per type, and the bit length on a wide screen
        a.live.c.type_len_short[5] = 3;
        a.live.c.type_len_bits[5] = Some(418);
        let s = render(&a, now, SystemTime::now(), 80, 40).text();
        assert!(s.contains("100.0%    3  30d \u{2502}"), "{s}");
        let s = render(&a, now, SystemTime::now(), 120, 40).text();
        assert!(s.contains("Len! Last LastLen"), "{s}");
        assert!(s.contains("    3  30d    418b (420-424)"), "{s}");
    }

    #[test]
    fn counts_fit_their_columns() {
        assert_eq!(fmt_count(9_999_999, 7), "9999999");
        assert_eq!(fmt_count(12_345_678, 7), "12.3M");
        assert_eq!(fmt_count(99_999_999, 7), "100.0M");
        assert_eq!(fmt_count(999_960_000, 7), "1.0G");
        assert_eq!(fmt_count(12_345, 4), "12k");
        assert_eq!(fmt_count(99_999, 4), "100k");
        assert_eq!(fmt_count(999_999, 4), "1.0M");
        assert_eq!(fmt_count(u64::MAX, 7), "18.4E");
        for n in [0, 9, 10_000, 999_500, 123_456_789, 10u64.pow(18), u64::MAX] {
            for w in 4..=10 {
                assert!(fmt_count(n, w).len() <= w, "{n} {w} {}", fmt_count(n, w));
            }
        }
    }

    /// Every view shows the frozen picture while paused, the footer says
    /// since when, and resuming shows live statistics again.
    #[test]
    fn paused_views_show_the_frozen_picture() {
        let t = Instant::now();
        let mut a = sample_app(t);
        let at = t + Duration::from_secs(60);
        a.pause(at, SystemTime::UNIX_EPOCH + Duration::from_secs(10));
        let ts = SystemTime::now();
        for i in 0..50 {
            let now = at + Duration::from_secs(i);
            a.line(
                ts,
                "!AIVDM,1,1,,A,15N4cJ`005Jrek0H@9n`DW5608EP,0*10".into(),
                now,
            );
        }
        assert_eq!(a.live.c.messages, 290);
        let later = at + Duration::from_secs(300);
        a.view = View::Main;
        let s = render(&a, later, SystemTime::now(), 100, 30).text();
        assert!(s.contains("Messages   240"), "{s}");
        assert!(s.contains("Uptime     1m00s"), "{s}");
        assert!(s.contains("PAUSED 00:00:10Z"), "{s}");
        assert!(s.contains("NMEA RECEIVED  PAUSED"), "{s}");
        a.view = View::Types;
        let s = render(&a, later, SystemTime::now(), 100, 30).text();
        assert!(s.contains(" 1 Pos A        120  50.0%        0s"), "{s}");
        assert!(s.contains("PAUSED 00:00:10Z"), "{s}");
        a.view = View::Mmsi;
        let s = render(&a, later, SystemTime::now(), 100, 30).text();
        assert!(s.contains("2 heard in 10m, 2 total"), "{s}");
        a.resume();
        let s = render(&a, later, SystemTime::now(), 100, 30).text();
        assert!(s.contains("3 total"), "{s}");
        assert!(!s.contains("PAUSED"), "{s}");
    }

    #[test]
    fn mmsi_table_scrolls_with_position() {
        let t = Instant::now();
        let mut a = App::new("x".into(), false, DEFAULT_WINDOW, t);
        let ts = SystemTime::now();
        let mut x: u64 = 1;
        for i in 0..40u32 {
            // vary the MMSI by re-armoring a type 1 header
            let mmsi = 244_000_000 + i;
            let bits = format!("{:06b}00{:030b}{}", 1, mmsi, "0".repeat(130));
            let payload: String = bits
                .as_bytes()
                .chunks(6)
                .map(|c| {
                    let v = u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap();
                    (if v < 40 { v + 48 } else { v + 56 }) as char
                })
                .collect();
            let body = format!("AIVDM,1,1,,A,{payload},0");
            let ck = body.bytes().fold(0u8, |a, b| a ^ b);
            x += 1;
            a.line(
                ts,
                format!("!{body}*{ck:02X}"),
                t + Duration::from_millis(x),
            );
        }
        assert_eq!(a.live.mmsi.len(), 40);
        a.view = View::Mmsi;
        let page = page_rows(&a, 24);
        assert_eq!(page, 19);
        let s = render(&a, t, SystemTime::now(), 80, 24).text();
        assert!(s.contains(" 1-19 of 40 "), "{s}");
        assert!(s.contains("244000039"), "{s}"); // newest first
        a.key(b'f', page, t);
        let s = render(&a, t, SystemTime::now(), 80, 24).text();
        assert!(s.contains(" 20-38 of 40 ") && s.contains("PAUSED"), "{s}");
        assert!(!s.contains("244000039"), "{s}");
        // scrolled past the end: the last page stays full
        a.key(b'f', page, t);
        let s = render(&a, t, SystemTime::now(), 80, 24).text();
        assert!(
            s.contains(" 22-40 of 40 ") && s.contains("244000000"),
            "{s}"
        );
    }

    #[test]
    fn axis_labels() {
        assert_eq!(fmt_age(90), "1m30s");
        assert_eq!(fmt_age(3600), "1h");
        assert_eq!(fmt_age(86400 + 1800), "1d30m");
        assert_eq!(fmt_age(0), "0s");
        assert_eq!(tail("abcdef", 6), "abcdef");
        assert_eq!(
            head("connected 127.0.0.1:10110", 25),
            "connected 127.0.0.1:10110"
        );
        assert_eq!(head("connected 127.0.0.1:10110", 14), "connected 1...");
        assert_eq!(
            (head("abcdef", 2), head("abcdef", 0)),
            ("..".into(), String::new())
        );
        assert_eq!(tail("replay:/very/long/path.nmea", 12), "...path.nmea");
        assert_eq!(tick_interval(1, 10), 10);
        assert_eq!(tick_interval(10, 10), 120);
        assert_eq!(tick_interval(60, 10), 600);
        assert_eq!(tick_interval(600, 10), 7200);
        assert_eq!(tick_interval(3600, 10), 43200);
    }

    #[test]
    fn timeline_columns() {
        let t = Instant::now();
        let a = sample_app(t);
        let now = t + Duration::from_secs(61);
        let cols = columns(&a.live, now, 10, 10);
        // seconds 0..60 have 4 msg/s; the newest column is second 60 alone
        assert_eq!(cols[9], Some(0.0));
        assert_eq!(cols[8], Some(4.0));
        assert_eq!(cols[3], Some(4.0));
        assert_eq!(cols[2], None);
        assert!(columns(&a.live, t, 1, 5).iter().all(Option::is_none));
    }
}
