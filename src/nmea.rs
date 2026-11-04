//! NMEA 0183 sentence splitting and checksum verification, plus the
//! IEC 61162-450 / NMEA 0183 v4.x tag block that may precede a sentence
//! (`\s:station,c:1700000000*hh\!AIVDM,...`).
//!
//! `split` is taken from gnomon (src/nmea/sentence.rs).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CkStatus {
    Ok,
    Missing,
    Bad { got: u8, want: u8 },
}

#[derive(Debug, Clone)]
pub struct Sentence<'a> {
    /// Address field without the leading '$'/'!', e.g. "AIVDM", "PSTT".
    pub addr: &'a str,
    /// Two-letter talker for standard sentences, None for proprietary.
    pub talker: Option<&'a str>,
    /// Formatter ("VDM", "VDO", ...); for proprietary sentences the full address.
    pub kind: &'a str,
    pub fields: Vec<&'a str>,
    pub ck: CkStatus,
}

fn xor(s: &str) -> u8 {
    s.bytes().fold(0u8, |a, b| a ^ b)
}

/// Checksum of `body` against the two hex digits that follow its '*'.
fn check(body: &str, hex: Option<&str>) -> CkStatus {
    let want = xor(body);
    match hex {
        None => CkStatus::Missing,
        Some(h) => match u8::from_str_radix(h, 16) {
            Ok(got) if got == want => CkStatus::Ok,
            Ok(got) => CkStatus::Bad { got, want },
            Err(_) => CkStatus::Bad { got: 0, want },
        },
    }
}

/// Split and checksum-verify one sentence. Returns None if it is not NMEA-shaped.
pub fn split(line: &str) -> Option<Sentence<'_>> {
    let line = line.trim_end_matches(['\r', '\n']);
    let first = line.as_bytes().first()?;
    // NMEA 0183 is printable ASCII by definition. Rejecting anything else
    // also keeps every byte-index slice below on a char boundary.
    if (*first != b'$' && *first != b'!') || !line.is_ascii() {
        return None;
    }
    let (body, ck) = match line.rfind('*') {
        Some(p) if line.len() >= p + 3 => {
            let body = &line[1..p];
            (body, check(body, Some(&line[p + 1..p + 3])))
        }
        Some(p) => (&line[1..p], CkStatus::Missing),
        None => (&line[1..], CkStatus::Missing),
    };
    let mut it = body.split(',');
    let addr = it.next()?;
    if addr.is_empty() || !addr.bytes().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let fields: Vec<&str> = it.collect();
    let (talker, kind) = if addr.starts_with('P') || addr.len() < 5 {
        (None, addr)
    } else {
        (Some(&addr[..2]), &addr[2..])
    };
    Some(Sentence {
        addr,
        talker,
        kind,
        fields,
        ck,
    })
}

/// A tag block in front of a sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct TagBlock<'a> {
    pub ck: CkStatus,
    /// `c:` UNIX time in seconds (values that only make sense as
    /// milliseconds, as some aggregators send, are scaled).
    pub time: Option<f64>,
    /// `s:` source / station identifier.
    pub source: Option<&'a str>,
}

/// Split a leading tag block off `line`. Returns the tag block (None when
/// the line has none) and the rest of the line. An unterminated tag block
/// is returned as is, so the caller sees a line that is not NMEA.
pub fn strip_tag(line: &str) -> (Option<TagBlock<'_>>, &str) {
    let Some(r) = line.strip_prefix('\\') else {
        return (None, line);
    };
    let Some((inner, rest)) = r.split_once('\\') else {
        return (None, line);
    };
    let (body, ck) = match inner.rfind('*') {
        Some(p) => (&inner[..p], check(&inner[..p], inner.get(p + 1..p + 3))),
        None => (inner, CkStatus::Missing),
    };
    let mut tb = TagBlock {
        ck,
        time: None,
        source: None,
    };
    for f in body.split(',') {
        match f.split_once(':') {
            Some(("c", v)) => {
                tb.time = v
                    .parse::<f64>()
                    .ok()
                    .filter(|t| t.is_finite() && *t > 0.0)
                    .map(|t| if t > 1e11 { t / 1000.0 } else { t });
            }
            Some(("s", v)) if !v.is_empty() => tb.source = Some(v),
            _ => {}
        }
    }
    (Some(tb), rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_and_split() {
        let s = split("!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5C").unwrap();
        assert_eq!(s.ck, CkStatus::Ok);
        assert_eq!((s.talker, s.kind), (Some("AI"), "VDM"));
        assert_eq!(s.fields.len(), 6);
        let s = split("!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5D").unwrap();
        assert_eq!(
            s.ck,
            CkStatus::Bad {
                got: 0x5D,
                want: 0x5C
            }
        );
        let s = split("!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0").unwrap();
        assert_eq!(s.ck, CkStatus::Missing);
        let s = split("$PSTT,1*00").unwrap();
        assert_eq!((s.talker, s.kind), (None, "PSTT"));
        assert!(split("hello").is_none());
        assert!(split("!AI\u{e9}VDM*00").is_none());
        assert!(split("!,1,2*00").is_none());
        assert!(split("").is_none());
    }

    #[test]
    fn tag_blocks() {
        let l = "\\s:r003669945,c:1241544035*79\\!AIVDM,1,1,,B,15N4cJ`005Jrek0H@9n`DW5608EP,0*13";
        let (tb, rest) = strip_tag(l);
        let tb = tb.unwrap();
        assert_eq!(tb.ck, CkStatus::Ok);
        assert_eq!(tb.time, Some(1241544035.0));
        assert_eq!(tb.source, Some("r003669945"));
        assert!(rest.starts_with("!AIVDM"));
        // milliseconds
        let (tb, _) = strip_tag("\\c:1241544035123*00\\!X");
        assert!((tb.unwrap().time.unwrap() - 1241544035.123).abs() < 1e-3);
        // no tag block / unterminated tag block
        assert_eq!(strip_tag("!AIVDM").0, None);
        let (tb, rest) = strip_tag("\\c:1*00!AIVDM");
        assert_eq!((tb, rest), (None, "\\c:1*00!AIVDM"));
        // garbage inside never panics
        let (tb, _) = strip_tag("\\*\\x");
        assert_eq!(tb.unwrap().ck, CkStatus::Missing);
        let (tb, _) = strip_tag("\\\u{e9}*\u{e9}\\x");
        assert!(matches!(tb.unwrap().ck, CkStatus::Bad { .. }));
    }
}
