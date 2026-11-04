//! Parsing of the input specification given on the command line.
//!
//!   /dev/ttyXXX            serial port
//!   host:port              raw NMEA over TCP (client)
//!   tcp://host:port        same, explicit
//!   [v6addr]:port          IPv6 literal
//!   udp://[addr]:port      listen for UDP datagrams; a multicast address
//!                          (experimental)
//!                          is joined (IEC 61162-450 style)
//!   udp://:port, udp://port  listen on 0.0.0.0

use std::net::IpAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Serial(String),
    Tcp { host: String, port: u16 },
    Udp { addr: IpAddr, port: u16 },
}

fn bracket(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

impl Target {
    /// Human readable form, IPv6 literals bracketed.
    pub fn display(&self) -> String {
        match self {
            Target::Serial(p) => p.clone(),
            Target::Tcp { host, port } => format!("tcp://{}:{port}", bracket(host)),
            Target::Udp { addr, port } => format!("udp://{}:{port}", bracket(&addr.to_string())),
        }
    }
}

/// Split "host:port", "[v6]:port", ":port" or "port" into (host, port).
fn host_port(s: &str) -> Result<(String, u16), String> {
    let (host, port) = if let Some(r) = s.strip_prefix('[') {
        let end = r.find(']').ok_or("unterminated '[' in IPv6 literal")?;
        let p = r[end + 1..]
            .strip_prefix(':')
            .ok_or("a port is required after an IPv6 literal")?;
        (r[..end].to_string(), p)
    } else {
        match s.rsplit_once(':') {
            Some((h, _)) if h.contains(':') => {
                return Err(format!("bracket IPv6 literals: [{h}]:port"));
            }
            Some((h, p)) => (h.to_string(), p),
            None => (String::new(), s),
        }
    };
    let port: u16 = port.parse().map_err(|_| format!("bad port: {port}"))?;
    if port == 0 {
        return Err("port 0 is not allowed".into());
    }
    Ok((host, port))
}

pub fn parse(spec: &str) -> Result<Target, String> {
    if spec.starts_with('/') {
        return Ok(Target::Serial(spec.to_string()));
    }
    if let Some(r) = spec.strip_prefix("udp://") {
        let (host, port) = host_port(r.trim_end_matches('/'))?;
        let addr = if host.is_empty() {
            IpAddr::from([0, 0, 0, 0])
        } else {
            host.parse()
                .map_err(|_| format!("udp:// needs an IP address, not {host}"))?
        };
        return Ok(Target::Udp { addr, port });
    }
    let r = spec.strip_prefix("tcp://").unwrap_or(spec);
    let r = r.trim_end_matches('/');
    if !r.contains(':') {
        return Err(format!("{spec}: a port is required (host:port)"));
    }
    let (host, port) = host_port(r)?;
    if host.is_empty() {
        return Err("empty host".into());
    }
    Ok(Target::Tcp { host, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        assert_eq!(
            parse("/dev/ttyUSB0").unwrap(),
            Target::Serial("/dev/ttyUSB0".into())
        );
        let t = parse("ais.example.net:10110").unwrap();
        assert_eq!(
            t,
            Target::Tcp {
                host: "ais.example.net".into(),
                port: 10110
            }
        );
        assert_eq!(t.display(), "tcp://ais.example.net:10110");
        let t = parse("tcp://[2001:db8::1]:4001").unwrap();
        assert_eq!(t.display(), "tcp://[2001:db8::1]:4001");
        assert!(parse("2001:db8::1").is_err());
        assert!(parse("host").is_err());
        assert!(parse("host:0").is_err());
        assert!(parse("host:x").is_err());
        assert!(parse(":10110").is_err());

        let t = parse("udp://:10110").unwrap();
        assert_eq!(t.display(), "udp://0.0.0.0:10110");
        assert_eq!(parse("udp://10110").unwrap(), t);
        let t = parse("udp://239.192.0.1:60001").unwrap();
        assert_eq!(t.display(), "udp://239.192.0.1:60001");
        let t = parse("udp://[::]:10110").unwrap();
        assert_eq!(t.display(), "udp://[::]:10110");
        assert!(parse("udp://localhost:10110").is_err());
        assert!(parse("udp://").is_err());
    }
}
