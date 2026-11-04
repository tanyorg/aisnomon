//! aisnomon: AIS / NMEA 0183 reception monitor.

use aisnomon::source::Ev;
use aisnomon::target::Target;
use aisnomon::{app, serial, source, target, term, ui};
use std::fs::OpenOptions;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

const USAGE: &str = "\
usage: aisnomon [options] target
       aisnomon [options] -f FILE

target:
  /dev/ttyXXX            serial port (speed from -s, else probe 38400,4800,...)
  host:port              raw NMEA over TCP (never writes to the peer)
  tcp://host:port        same, explicit; [ipv6]:port for IPv6 literals
  udp://[addr]:port      listen for UDP; a multicast group is joined
  udp://:port            listen on 0.0.0.0

options:
  -s, --speed BPS        serial speed
  -l, --log FILE         append every received line to FILE
  -f, --replay FILE      play back a capture ('-' = stdin, unpaced; a file is
                         paced by tag block c: times when present)
  -w, --window SECS      'recently heard' window for the MMSI count
                         (default 600, 10 to 86400)
  -a, --ascii            ASCII box drawing (for ambiguous-width terminals)
      --snapshot SECS    no TUI: run SECS seconds, print one screen, exit
                         (0 < SECS <= 86400)
      --size WxH         canvas size for --snapshot (default 100x40,
                         80x24 to 1000x1000)
      --view N           view for --snapshot: 1 main, 2 types, 3 MMSI, 4 raw
  -h, --help             this help
  -V, --version          version

keys: q quit, 1-4 views, p pause, j/k or arrows scroll, b/f or PgUp/PgDn page,
      t timeline scale, s MMSI sort, c clear statistics";

struct Opts {
    target: Option<String>,
    speed: Option<u32>,
    log: Option<String>,
    replay: Option<String>,
    window: Duration,
    ascii: bool,
    snapshot: Option<Duration>,
    size: (usize, usize),
    view: u8,
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        target: None,
        speed: None,
        log: None,
        replay: None,
        window: app::DEFAULT_WINDOW,
        ascii: false,
        snapshot: None,
        size: (100, 40),
        view: b'1',
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name} needs an argument"));
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("aisnomon {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-a" | "--ascii" => o.ascii = true,
            "-s" | "--speed" => {
                let v = val("--speed")?;
                let b: u32 = v.parse().map_err(|_| format!("bad speed: {v}"))?;
                if b < 300 {
                    return Err(format!("bad speed: {v}"));
                }
                o.speed = Some(b);
            }
            "-w" | "--window" => {
                let v = val("--window")?;
                let s: u64 = v.parse().map_err(|_| format!("bad --window: {v}"))?;
                if !(10..=86400).contains(&s) {
                    return Err(format!("--window must be in [10, 86400]: {v}"));
                }
                o.window = Duration::from_secs(s);
            }
            "-l" | "--log" => o.log = Some(val("--log")?),
            "-f" | "--replay" => o.replay = Some(val("--replay")?),
            "--snapshot" => o.snapshot = Some(term::parse_snapshot(&val("--snapshot")?)?),
            "--size" => o.size = term::parse_size(&val("--size")?, (ui::MIN_W, ui::MIN_H))?,
            "--view" => {
                let v = val("--view")?;
                match v.as_bytes() {
                    [d @ b'1'..=b'4'] => o.view = *d,
                    _ => return Err(format!("--view must be 1-4: {v}")),
                }
            }
            s if s.starts_with('-') && s != "-" => return Err(format!("unknown option {s}")),
            s => {
                if o.target.replace(s.to_string()).is_some() {
                    return Err("only one target may be given".into());
                }
            }
        }
    }
    Ok(o)
}

fn main() {
    let o = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("aisnomon: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(o) {
        eprintln!("aisnomon: {e}");
        std::process::exit(1);
    }
}

fn run(o: Opts) -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<Ev>();

    let label = if let Some(path) = &o.replay {
        if o.target.is_some() {
            return Err("--replay and a target are mutually exclusive".into());
        }
        if o.speed.is_some() || o.log.is_some() {
            return Err("--speed and --log do not apply to --replay".into());
        }
        source::spawn_replay(path.clone(), tx.clone());
        format!("replay:{path}")
    } else {
        let spec = o.target.as_deref().ok_or("no target given")?;
        let t = target::parse(spec)?;
        if o.speed.is_some() && !matches!(t, Target::Serial(_)) {
            return Err("--speed only applies to a serial device".into());
        }
        let log = match &o.log {
            Some(p) => Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .map_err(|e| format!("{p}: {e}"))?,
            ),
            None => None,
        };
        let label = t.display();
        match t {
            Target::Serial(path) => serial::spawn(path, o.speed, log, tx.clone()),
            Target::Tcp { host, port } => source::spawn_tcp(host, port, log, tx.clone()),
            Target::Udp { addr, port } => source::spawn_udp(addr, port, log, tx.clone()),
        }
        label
    };

    let mut app = app::App::new(label, o.ascii, o.window, Instant::now());

    if let Some(d) = o.snapshot {
        term::drain_for(&rx, d, |ev| {
            handle(&mut app, ev, 1);
        });
        app.key(o.view, 1, Instant::now());
        app.tick(Instant::now());
        let c = ui::render(&app, Instant::now(), SystemTime::now(), o.size.0, o.size.1);
        print!("{}", c.text());
        return Ok(());
    }

    term::require_tty()?;
    if o.replay.as_deref() == Some("-") {
        return Err("--replay - needs stdin, which is also the keyboard; use --snapshot".into());
    }
    // `app` is used by both closures; the loop calls them one at a time.
    let app = std::cell::RefCell::new(app);
    term::run_tui(
        &tx,
        &rx,
        Duration::from_millis(100),
        Ev::Key,
        |ev| {
            let page = ui::raw_rows(&app.borrow(), term::size().1);
            handle(&mut app.borrow_mut(), ev, page)
        },
        |w, h| {
            let mut a = app.borrow_mut();
            a.tick(Instant::now());
            ui::render(&a, Instant::now(), SystemTime::now(), w, h)
        },
    )
}

/// Returns false when the user asked to quit.
fn handle(app: &mut app::App, ev: Ev, page: usize) -> bool {
    match ev {
        Ev::Line(ts, l) => app.line(ts, l, Instant::now()),
        Ev::Status(s) => app.status = s,
        Ev::Connected(desc, kind) => {
            app.connected = true;
            app.kind = kind;
            app.status = desc;
        }
        Ev::Disconnected(why) => {
            if app.connected {
                app.reconnects += 1;
            }
            app.connected = false;
            app.status = why;
        }
        Ev::Key(k) => return app.key(k, page, Instant::now()),
        Ev::Idle => {}
    }
    true
}
