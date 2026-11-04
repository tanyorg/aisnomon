//! Data sources: raw NMEA over TCP, UDP datagrams and file/stdin replay.
//! The serial source lives in `serial.rs`.
//! Each source runs on its own thread and reports through an mpsc channel.
//! No source ever writes to the receiver.
//!
//! The TCP source and the line splitter are adapted from gnomon
//! (src/source.rs), with the gpsd protocol removed.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Unknown,
    Tcp,
    Udp,
    Replay,
    /// Direct serial port. `cdc`: USB CDC-ACM (nominal speed, no wire timing).
    Serial {
        speed: u32,
        cdc: bool,
    },
}

#[derive(Debug)]
pub enum Ev {
    Line(SystemTime, String),
    Status(String),
    Connected(String, Kind),
    Disconnected(String),
    Key(u8),
    /// Sent by a quiet source so it notices when the UI has gone.
    Idle,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// AIS traffic can legitimately stop for minutes in a quiet area, so the
/// TCP silence timeout (which catches half-open connections) is long.
pub const TCP_SILENCE: Duration = Duration::from_secs(300);
pub const MAX_LINE: usize = 8192;
/// IEC 61162-450 datagram header.
const UDPBC: &[u8] = b"UdPbC\0";
/// Distinct UDP senders reported in the status line.
const MAX_PEERS: usize = 16;

pub struct LineSplitter {
    buf: Vec<u8>,
}

impl LineSplitter {
    pub fn new() -> Self {
        LineSplitter { buf: Vec::new() }
    }

    pub fn push(&mut self, data: &[u8], mut out: impl FnMut(String)) {
        for &b in data {
            match b {
                b'\n' => {
                    let mut l = std::mem::take(&mut self.buf);
                    if l.last() == Some(&b'\r') {
                        l.pop();
                    }
                    if !l.is_empty() {
                        out(String::from_utf8_lossy(&l).into_owned());
                    }
                }
                _ => {
                    if self.buf.len() < MAX_LINE {
                        self.buf.push(b);
                    }
                }
            }
        }
    }
}

impl Default for LineSplitter {
    fn default() -> Self {
        Self::new()
    }
}

/// Run `session` forever with a 1..8 s back-off between attempts. The
/// back-off resets after a session that delivered data.
pub(crate) fn retry_loop(
    tx: &Sender<Ev>,
    mut session: impl FnMut(&mut bool) -> Result<String, String>,
) {
    let mut backoff = 1u64;
    loop {
        let mut got_data = false;
        let reason = session(&mut got_data).unwrap_or_else(|e| e);
        if got_data {
            backoff = 1;
        }
        if tx.send(Ev::Disconnected(reason.clone())).is_err() {
            return;
        }
        for left in (1..=backoff).rev() {
            if tx
                .send(Ev::Status(format!("{reason}; retry in {left}s")))
                .is_err()
            {
                return;
            }
            thread::sleep(Duration::from_secs(1));
        }
        backoff = (backoff * 2).min(8);
    }
}

// ---------------------------------------------------------------- TCP ---

pub fn spawn_tcp(host: String, port: u16, log: Option<File>, tx: Sender<Ev>) {
    thread::spawn(move || {
        let mut log = log;
        retry_loop(&tx.clone(), |got| {
            tcp_session(&host, port, &mut log, &tx, got).map_err(|e| e.to_string())
        });
    });
}

fn connect(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let mut last = std::io::Error::new(ErrorKind::NotFound, "no addresses");
    for a in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = std::io::Error::new(e.kind(), format!("{a}: {e}")),
        }
    }
    Err(last)
}

/// One connection lifetime. Returns a human readable reason when it ends.
fn tcp_session(
    host: &str,
    port: u16,
    log: &mut Option<File>,
    tx: &Sender<Ev>,
    got_data: &mut bool,
) -> std::io::Result<String> {
    let label = crate::target::Target::Tcp {
        host: host.to_string(),
        port,
    }
    .display();
    let _ = tx.send(Ev::Status(format!("connecting to {label}")));
    let mut sock = connect(host, port)?;
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    let peer = sock.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let _ = tx.send(Ev::Connected(format!("connected {peer}"), Kind::Tcp));

    let mut last_rx = Instant::now();
    let mut split = LineSplitter::new();
    let mut buf = [0u8; 4096];
    loop {
        match sock.read(&mut buf) {
            Ok(0) => return Ok("connection closed by peer".into()),
            Ok(n) => {
                last_rx = Instant::now();
                *got_data = true;
                let data = &buf[..n];
                if let Some(f) = log.as_mut() {
                    let _ = f.write_all(data);
                }
                let now = SystemTime::now();
                let mut gone = false;
                split.push(data, |l| gone |= tx.send(Ev::Line(now, l)).is_err());
                if gone {
                    return Ok("ui gone".into());
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if last_rx.elapsed() > TCP_SILENCE {
                    return Ok(format!("no data for {}s", TCP_SILENCE.as_secs()));
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

// ---------------------------------------------------------------- UDP ---

/// Lines in one datagram. Datagrams are self-contained: a missing final
/// newline still ends the last sentence, and nothing carries over.
pub fn datagram_lines(mut data: &[u8]) -> Vec<String> {
    if let Some(r) = data.strip_prefix(UDPBC) {
        data = r;
    }
    data.split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .filter(|l| !l.is_empty())
        .map(|l| String::from_utf8_lossy(&l[..l.len().min(MAX_LINE)]).into_owned())
        .collect()
}

fn udp_bind(addr: IpAddr, port: u16) -> std::io::Result<UdpSocket> {
    let s = match addr {
        IpAddr::V4(a) if a.is_multicast() => {
            let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))?;
            s.join_multicast_v4(&a, &Ipv4Addr::UNSPECIFIED)?;
            s
        }
        IpAddr::V6(a) if a.is_multicast() => {
            let s = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, port))?;
            s.join_multicast_v6(&a, 0)?;
            s
        }
        a => UdpSocket::bind(SocketAddr::new(a, port))?,
    };
    s.set_read_timeout(Some(Duration::from_millis(500)))?;
    Ok(s)
}

pub fn spawn_udp(addr: IpAddr, port: u16, log: Option<File>, tx: Sender<Ev>) {
    thread::spawn(move || {
        let mut log = log;
        retry_loop(&tx.clone(), |got| {
            udp_session(addr, port, &mut log, &tx, got).map_err(|e| e.to_string())
        });
    });
}

fn udp_session(
    addr: IpAddr,
    port: u16,
    log: &mut Option<File>,
    tx: &Sender<Ev>,
    got_data: &mut bool,
) -> std::io::Result<String> {
    let label = crate::target::Target::Udp { addr, port }.display();
    let sock =
        udp_bind(addr, port).map_err(|e| std::io::Error::new(e.kind(), format!("{label}: {e}")))?;
    let _ = tx.send(Ev::Connected(format!("listening {label}"), Kind::Udp));
    let mut peers: HashSet<SocketAddr> = HashSet::new();
    let mut buf = vec![0u8; 65536];
    loop {
        match sock.recv_from(&mut buf) {
            Ok((n, peer)) => {
                *got_data = true;
                let data = &buf[..n];
                if peers.len() < MAX_PEERS && peers.insert(peer) {
                    let more = if peers.len() > 1 {
                        format!(" ({} senders)", peers.len())
                    } else {
                        String::new()
                    };
                    let _ = tx.send(Ev::Status(format!("datagrams from {peer}{more}")));
                }
                let lines = datagram_lines(data);
                if let Some(f) = log.as_mut() {
                    for l in &lines {
                        let _ = writeln!(f, "{l}\r");
                    }
                }
                let now = SystemTime::now();
                for l in lines {
                    if tx.send(Ev::Line(now, l)).is_err() {
                        return Ok("ui gone".into());
                    }
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                // Detect a dead UI even when no traffic arrives.
                if tx.send(Ev::Idle).is_err() {
                    return Ok("ui gone".into());
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

// ------------------------------------------------------------- replay ---

/// Replay a capture. A path of "-" reads stdin unpaced (e.g. `nc host 10110 |`).
/// A file is paced by tag block `c:` times when present; lines without a
/// tag block time are delivered immediately.
pub fn spawn_replay(path: String, tx: Sender<Ev>) {
    thread::spawn(move || {
        let reader: Box<dyn BufRead> = if path == "-" {
            Box::new(BufReader::new(std::io::stdin()))
        } else {
            match File::open(&path) {
                Ok(f) => Box::new(BufReader::new(f)),
                Err(e) => {
                    let _ = tx.send(Ev::Disconnected(format!("{path}: {e}")));
                    return;
                }
            }
        };
        let paced = path != "-";
        let _ = tx.send(Ev::Connected(format!("replay {path}"), Kind::Replay));
        let mut prev: Option<f64> = None;
        for line in reader.split(b'\n') {
            let Ok(mut l) = line else { break };
            if l.last() == Some(&b'\r') {
                l.pop();
            }
            if l.is_empty() {
                continue;
            }
            l.truncate(MAX_LINE);
            let s = String::from_utf8_lossy(&l).into_owned();
            if paced {
                if let Some(t) = crate::nmea::strip_tag(&s).0.and_then(|tb| tb.time) {
                    if let Some(p) = prev {
                        let d = t - p;
                        if d > 1e-6 {
                            thread::sleep(Duration::from_secs_f64(d.min(5.0)));
                        }
                    }
                    prev = Some(t);
                }
            }
            if tx.send(Ev::Line(SystemTime::now(), s)).is_err() {
                return;
            }
        }
        let _ = tx.send(Ev::Disconnected("end of input".into()));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter() {
        let mut sp = LineSplitter::new();
        let mut v = Vec::new();
        sp.push(b"!AIA*00\r\n!AI", |l| v.push(l));
        sp.push(b"B*00\r\n\r\n", |l| v.push(l));
        assert_eq!(v, vec!["!AIA*00", "!AIB*00"]);
    }

    #[test]
    fn datagrams() {
        assert_eq!(datagram_lines(b"!A*00"), vec!["!A*00"]);
        assert_eq!(
            datagram_lines(b"!A*00\r\n!B*00\r\n\r\n"),
            vec!["!A*00", "!B*00"]
        );
        assert_eq!(
            datagram_lines(b"UdPbC\0\\s:x*00\\!A*00\r\n"),
            vec!["\\s:x*00\\!A*00"]
        );
        assert!(datagram_lines(b"").is_empty());
        assert!(datagram_lines(b"\r\n").is_empty());
    }

    #[test]
    fn udp_loopback() {
        let (tx, rx) = std::sync::mpsc::channel();
        let rxs = udp_bind(IpAddr::from([127, 0, 0, 1]), 0);
        let Ok(rxs) = rxs else { return }; // no loopback in this sandbox
        let port = rxs.local_addr().unwrap().port();
        drop(rxs);
        spawn_udp(IpAddr::from([127, 0, 0, 1]), port, None, tx);
        // wait for the bind
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match rx.recv_timeout(Duration::from_secs(3)).unwrap() {
                Ev::Connected(..) => break,
                _ if Instant::now() > deadline => panic!("no bind"),
                _ => {}
            }
        }
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.send_to(b"!AIVDM,1,1,,A,x,0*00\r\n", ("127.0.0.1", port))
            .unwrap();
        loop {
            match rx.recv_timeout(Duration::from_secs(3)).unwrap() {
                Ev::Line(_, l) => {
                    assert_eq!(l, "!AIVDM,1,1,,A,x,0*00");
                    break;
                }
                _ if Instant::now() > deadline => panic!("no line"),
                _ => {}
            }
        }
    }
}
