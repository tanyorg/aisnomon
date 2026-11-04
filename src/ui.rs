//! Screen layout. Main view: SUMMARY, RECEPTION TIMELINE, NMEA RECEIVED.
//! Detail views: channels/types/errors, MMSI table, full-height raw log.

use crate::ais;
use crate::app::{fmt_dur, fmt_tod, fmt_utc, unix, App, RawLine, View, SCALES};
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

fn pct(n: u64, d: u64) -> String {
    if d == 0 {
        "-".into()
    } else {
        format!("{:.1}%", n as f64 * 100.0 / d as f64)
    }
}

/// Compact age for axis labels: 30s, 5m, 2h, 1d.
fn fmt_age(s: u64) -> String {
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
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
    let body = h - 2;
    match app.view {
        View::Main => {
            summary(&mut c, app, now, 1, w);
            let rest = body - SUMMARY_H;
            let tl = (rest * 2 / 5).clamp(6, 14);
            timeline(&mut c, app, now, 1 + SUMMARY_H, w, tl);
            raw_box(&mut c, app, 1 + SUMMARY_H + tl, w, rest - tl);
        }
        View::Types => types_view(&mut c, app, now, 1, w, body),
        View::Mmsi => mmsi_view(&mut c, app, now, 1, w, body),
        View::Raw => raw_box(&mut c, app, 1, w, body),
    }
    footer(&mut c, app, h - 1);
    c
}

/// Rows available for the raw log, for page-wise scrolling.
pub fn raw_rows(app: &App, h: usize) -> usize {
    if h < MIN_H {
        return 1;
    }
    match app.view {
        View::Raw => h - 4,
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
    x = c.put(x, 0, &format!("  {}", app.target), 0);
    let clock = fmt_utc(wall);
    let room = c.w.saturating_sub(x + clock.len() + 4);
    let status: String = app.status.chars().take(room).collect();
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
    let keys = match app.view {
        View::Mmsi => " s sort  c clear  q quit",
        _ => " p pause  j/k scroll  t scale  c clear  q quit",
    };
    c.put(x, y, keys, st::DIM);
}

fn field(c: &mut Canvas, x: usize, y: usize, label: &str, value: &str, style: u8) {
    c.put(x, y, label, st::DIM);
    c.put(x + 11, y, value, style);
}

fn summary(c: &mut Canvas, app: &App, now: Instant, y: usize, w: usize) {
    c.boxed(0, y, w, SUMMARY_H, "SUMMARY");
    let k = &app.c;
    let cw = (w - 4) / 3;
    let (x1, x2, x3) = (2, 2 + cw, 2 + 2 * cw);
    let y = y + 1;

    field(c, x1, y, "Messages", &k.messages.to_string(), st::BOLD);
    field(
        c,
        x1,
        y + 1,
        "Rate 10s",
        &fmt_rate(app.timeline.rate(now, 10)),
        0,
    );
    field(
        c,
        x1,
        y + 2,
        "Rate 1m",
        &fmt_rate(app.timeline.rate(now, 60)),
        0,
    );
    let (last, style) = match app.last_msg {
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
        &fmt_dur(now.saturating_duration_since(app.started)),
        0,
    );

    let total = k.messages;
    let chan = |n: u64| format!("{n:<8} {}", pct(n, total));
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
        &format!("MMSI {}", window_label(app.window)),
        &app.mmsi_recent(now).to_string(),
        st::BOLD,
    );
    let all = if app.mmsi_evicted > 0 {
        format!("{}+", app.mmsi.len() as u64 + app.mmsi_evicted)
    } else {
        app.mmsi.len().to_string()
    };
    field(c, x2, y + 4, "MMSI all", &all, 0);

    let snt = k.valid + k.ck_bad + k.ck_missing;
    let bad = |n: u64, s: u8| if n > 0 { s } else { 0 };
    field(
        c,
        x3,
        y,
        "Valid",
        &format!("{:<8} {}", k.valid, pct(k.valid, snt)),
        0,
    );
    field(
        c,
        x3,
        y + 1,
        "CK error",
        &format!(
            "{:<8} {}",
            k.ck_bad + k.ck_missing,
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
pub fn columns(app: &App, now: Instant, scale: u64, n: usize) -> Vec<Option<f64>> {
    let cur = app.timeline.sec(now);
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
        let sum = app.timeline.sum(start, end)[0];
        *slot = Some(sum as f64 / (end - start) as f64);
    }
    out
}

const BARS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];

fn timeline(c: &mut Canvas, app: &App, now: Instant, y: usize, w: usize, h: usize) {
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
    let cols = columns(app, now, scale, ncol);
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
    let step = 12;
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

fn raw_box(c: &mut Canvas, app: &App, y: usize, w: usize, h: usize) {
    let title = match app.paused {
        Some(_) if app.scroll > 0 => format!(
            "NMEA RECEIVED  PAUSED, {} lines back  [p] resume  [j/k] scroll",
            app.scroll
        ),
        Some(_) => "NMEA RECEIVED  PAUSED  [p] resume  [j/k] scroll".to_string(),
        None => "NMEA RECEIVED  [p] pause".to_string(),
    };
    c.boxed(0, y, w, h, &title);
    if h < 3 {
        return;
    }
    let n = h - 2;
    let end = app
        .paused
        .unwrap_or(app.raw.len())
        .saturating_sub(app.scroll);
    let start = end.saturating_sub(n);
    for (i, l) in app.raw.range(start..end).enumerate() {
        raw_line(c, l, 2, y + 1 + i, w - 4);
    }
}

fn types_view(c: &mut Canvas, app: &App, now: Instant, y: usize, w: usize, h: usize) {
    let k = &app.c;
    let lw = 40.min(w / 2);
    c.boxed(0, y, lw, h, "CHANNELS & INTEGRITY");
    let cur = app.timeline.sec(now);
    let span = cur.min(600);
    let recent = app.timeline.sum(cur - span, cur);
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
                k.chan[i],
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
        ("Pending frags", app.pending_frags() as u64, 0),
    ];
    for (label, v, s) in rows {
        if yy >= y + h - 1 {
            break;
        }
        c.put(2, yy, label, st::DIM);
        c.put(22, yy, &format!("{v:>10}"), if v > 0 { s } else { 0 });
        yy += 1;
    }

    let rw = w - lw;
    c.boxed(lw, y, rw, h, "MESSAGE TYPES");
    c.put(
        lw + 2,
        y + 1,
        "Type  Name           Msgs       %  Last",
        st::DIM,
    );
    let mut yy = y + 2;
    for t in 1..=27u8 {
        let n = k.types[t as usize];
        if n == 0 {
            continue;
        }
        if yy >= y + h - 1 {
            break;
        }
        let last = k.type_last[t as usize]
            .map(|l| fmt_dur(now.saturating_duration_since(l)))
            .unwrap_or_default();
        c.put(
            lw + 2,
            yy,
            &format!(
                "{t:>4}  {:<10} {n:>9} {:>7}  {last}",
                ais::type_name(t),
                pct(n, k.messages)
            ),
            0,
        );
        yy += 1;
    }
}

fn mmsi_view(c: &mut Canvas, app: &App, now: Instant, y: usize, w: usize, h: usize) {
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
            app.mmsi_recent(now),
            window_label(app.window),
            app.mmsi.len()
        ),
    );
    c.put(
        2,
        y + 1,
        "MMSI       Kind       Msgs      A      B  Interval    Last  Types",
        st::DIM,
    );
    let mut v: Vec<_> = app.mmsi.iter().collect();
    if app.mmsi_by_count {
        v.sort_by(|a, b| b.1.count.cmp(&a.1.count).then(a.0.cmp(b.0)));
    } else {
        v.sort_by(|a, b| b.1.last.cmp(&a.1.last).then(a.0.cmp(b.0)));
    }
    let rows = h.saturating_sub(3);
    for (i, (mmsi, m)) in v.iter().take(rows).enumerate() {
        let age = now.saturating_duration_since(m.last);
        let style = if age > app.window { st::DIM } else { 0 };
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
        c.put(
            w - 16,
            y + h - 1,
            &format!(" {} more ", v.len() - rows),
            st::DIM,
        );
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
                    raw_rows(&a, h);
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
    }

    #[test]
    fn timeline_columns() {
        let t = Instant::now();
        let a = sample_app(t);
        let now = t + Duration::from_secs(61);
        let cols = columns(&a, now, 10, 10);
        // seconds 0..60 have 4 msg/s; the newest column is second 60 alone
        assert_eq!(cols[9], Some(0.0));
        assert_eq!(cols[8], Some(4.0));
        assert_eq!(cols[3], Some(4.0));
        assert_eq!(cols[2], None);
        assert!(columns(&a, t, 1, 5).iter().all(Option::is_none));
    }
}
