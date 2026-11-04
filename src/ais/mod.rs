//! AIS over NMEA 0183: VDM/VDO encapsulation (IEC 61162-1), 6-bit payload
//! armoring and the message header common to all ITU-R M.1371 messages
//! (type, repeat indicator, MMSI). Message bodies are not decoded: aisnomon
//! observes reception, it does not plot vessels.

pub mod frag;

use crate::nmea::Sentence;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chan {
    A,
    B,
    /// Empty or non-standard channel field.
    Unknown,
}

impl Chan {
    /// 'A'/'B' per IEC 61162-1; some receivers send '1'/'2'.
    pub fn parse(s: &str) -> Chan {
        match s {
            "A" | "1" => Chan::A,
            "B" | "2" => Chan::B,
            _ => Chan::Unknown,
        }
    }

    pub fn letter(self) -> char {
        match self {
            Chan::A => 'A',
            Chan::B => 'B',
            Chan::Unknown => '-',
        }
    }

    pub fn index(self) -> usize {
        match self {
            Chan::A => 0,
            Chan::B => 1,
            Chan::Unknown => 2,
        }
    }
}

/// One VDM (received) or VDO (own station) sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vdm<'a> {
    /// VDO: own-ship data, not something received over the air.
    pub own: bool,
    pub count: u8,
    pub num: u8,
    pub seq: Option<u8>,
    pub chan: Chan,
    pub payload: &'a str,
    pub fill: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VdmErr {
    Fields,
    Count,
    Num,
    Seq,
    Fill,
    EmptyPayload,
}

impl fmt::Display for VdmErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            VdmErr::Fields => "field count",
            VdmErr::Count => "fragment count",
            VdmErr::Num => "fragment number",
            VdmErr::Seq => "sequence id",
            VdmErr::Fill => "fill bits",
            VdmErr::EmptyPayload => "empty payload",
        })
    }
}

pub fn is_vdm(s: &Sentence) -> bool {
    s.talker.is_some() && matches!(s.kind, "VDM" | "VDO")
}

/// Parse the encapsulation fields of a VDM/VDO sentence
/// (`count,num,seq,chan,payload,fill`). The payload is not de-armored here.
pub fn parse_vdm<'a>(s: &Sentence<'a>) -> Result<Vdm<'a>, VdmErr> {
    let f = &s.fields;
    if f.len() < 6 {
        return Err(VdmErr::Fields);
    }
    let digit = |v: &str| -> Option<u8> {
        (v.len() == 1)
            .then(|| v.as_bytes()[0])
            .filter(u8::is_ascii_digit)
            .map(|b| b - b'0')
    };
    let count = digit(f[0]).filter(|&c| c >= 1).ok_or(VdmErr::Count)?;
    let num = digit(f[1])
        .filter(|&n| (1..=count).contains(&n))
        .ok_or(VdmErr::Num)?;
    let seq = match f[2] {
        "" => None,
        v => Some(digit(v).ok_or(VdmErr::Seq)?),
    };
    // The fill field may be empty on non-final fragments.
    let fill = match f[5] {
        "" if num < count => 0,
        v => digit(v).filter(|&b| b <= 5).ok_or(VdmErr::Fill)?,
    };
    if f[4].is_empty() {
        return Err(VdmErr::EmptyPayload);
    }
    Ok(Vdm {
        own: s.kind == "VDO",
        count,
        num,
        seq,
        chan: Chan::parse(f[3]),
        payload: f[4],
        fill,
    })
}

/// A de-armored payload: one 6-bit value per character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bits {
    sextets: Vec<u8>,
    /// Payload length in bits (fill bits removed).
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmorErr {
    /// Character outside '0'..'W' and '`'..'w'.
    BadChar(char),
    /// More fill bits than the payload has.
    Fill,
}

impl fmt::Display for ArmorErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ArmorErr::BadChar(c) => write!(f, "bad armor char {c:?}"),
            ArmorErr::Fill => f.write_str("fill exceeds payload"),
        }
    }
}

pub fn dearmor(payload: &str, fill: u8) -> Result<Bits, ArmorErr> {
    let mut sextets = Vec::with_capacity(payload.len());
    for c in payload.chars() {
        let v = match c {
            '0'..='W' => c as u8 - 48,
            '`'..='w' => c as u8 - 56,
            _ => return Err(ArmorErr::BadChar(c)),
        };
        sextets.push(v);
    }
    let len = (sextets.len() * 6)
        .checked_sub(fill as usize)
        .ok_or(ArmorErr::Fill)?;
    Ok(Bits { sextets, len })
}

impl Bits {
    /// Unsigned field of `width` (<= 32) bits starting at bit `start`.
    pub fn uint(&self, start: usize, width: usize) -> Option<u32> {
        if width > 32 || start + width > self.len {
            return None;
        }
        let mut v = 0u32;
        for i in start..start + width {
            let bit = (self.sextets[i / 6] >> (5 - i % 6)) & 1;
            v = (v << 1) | bit as u32;
        }
        Some(v)
    }
}

/// Plausibility of the payload length for the message type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LenCheck {
    Ok,
    Short,
    Long,
}

/// The header shared by all messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub mtype: u8,
    pub repeat: u8,
    pub mmsi: u32,
    pub bits: usize,
    pub len: LenCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderErr {
    /// Fewer than the 38 header bits.
    Short(usize),
    /// Type 0 or above 27.
    Type(u8),
}

impl fmt::Display for HeaderErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HeaderErr::Short(n) => write!(f, "only {n} bits"),
            HeaderErr::Type(t) => write!(f, "message type {t}"),
        }
    }
}

/// Accepted payload lengths in bits per message type (ITU-R M.1371-5),
/// with the common real-world deviations tolerated: type 5 at 420 bits
/// (missing spare/fill handling), type 24 part A at 160 or 168, and the
/// variable-length binary and safety messages bounded by five slots.
pub fn len_range(t: u8) -> (usize, usize) {
    match t {
        1..=3 | 9 | 11 | 18 | 4 | 22 => (168, 168),
        5 => (420, 424),
        6 => (88, 1008),
        7 | 13 => (72, 168),
        8 => (56, 1008),
        10 => (72, 72),
        12 => (72, 1008),
        14 => (40, 1008),
        15 => (88, 160),
        16 => (96, 144),
        17 => (80, 816),
        19 => (312, 312),
        20 => (72, 160),
        21 => (272, 360),
        23 => (160, 160),
        24 => (160, 168),
        25 => (40, 168),
        26 => (60, 1064),
        27 => (96, 168),
        _ => (0, usize::MAX),
    }
}

pub fn header(b: &Bits) -> Result<Header, HeaderErr> {
    if b.len < 38 {
        return Err(HeaderErr::Short(b.len));
    }
    let mtype = b.uint(0, 6).unwrap_or(0) as u8;
    if !(1..=27).contains(&mtype) {
        return Err(HeaderErr::Type(mtype));
    }
    let (lo, hi) = len_range(mtype);
    let len = if b.len < lo {
        LenCheck::Short
    } else if b.len > hi {
        LenCheck::Long
    } else {
        LenCheck::Ok
    };
    Ok(Header {
        mtype,
        repeat: b.uint(6, 2).unwrap_or(0) as u8,
        mmsi: b.uint(8, 30).unwrap_or(0),
        bits: b.len,
        len,
    })
}

/// Short message type names (fit in 10 columns).
pub fn type_name(t: u8) -> &'static str {
    match t {
        1..=3 => "Pos A",
        4 => "Base stn",
        5 => "Static A",
        6 => "Addr bin",
        7 => "Bin ack",
        8 => "Bcast bin",
        9 => "SAR air",
        10 => "UTC inq",
        11 => "UTC resp",
        12 => "Addr SRM",
        13 => "SRM ack",
        14 => "Bcast SRM",
        15 => "Interrog",
        16 => "Asgn mode",
        17 => "DGNSS",
        18 => "Pos B",
        19 => "Ext pos B",
        20 => "Link mgmt",
        21 => "AtoN",
        22 => "Chan mgmt",
        23 => "Grp asgn",
        24 => "Static B",
        25 => "1slot bin",
        26 => "Mslot bin",
        27 => "Long rng",
        _ => "?",
    }
}

/// Station category from the MMSI digit pattern (ITU-R M.585).
pub fn mmsi_kind(m: u32) -> &'static str {
    match m {
        0 => "invalid",
        1_000_000..=9_999_999 => "coast",   // 00MIDxxxx
        10_000_000..=99_999_999 => "group", // 0MIDxxxxx
        111_000_000..=111_999_999 => "SAR air",
        200_000_000..=799_999_999 => "ship",
        800_000_000..=899_999_999 => "handheld",
        970_000_000..=970_999_999 => "SART",
        972_000_000..=972_999_999 => "MOB",
        974_000_000..=974_999_999 => "EPIRB",
        980_000_000..=989_999_999 => "craft",
        990_000_000..=999_999_999 => "AtoN",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nmea::split;

    #[test]
    fn single_part_position_report() {
        let s = split("!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5C").unwrap();
        assert!(is_vdm(&s));
        let v = parse_vdm(&s).unwrap();
        assert_eq!(
            (v.own, v.count, v.num, v.seq, v.chan, v.fill),
            (false, 1, 1, None, Chan::B, 0)
        );
        let b = dearmor(v.payload, v.fill).unwrap();
        let h = header(&b).unwrap();
        assert_eq!(
            (h.mtype, h.repeat, h.mmsi, h.bits, h.len),
            (1, 0, 477553000, 168, LenCheck::Ok)
        );
        assert_eq!(mmsi_kind(h.mmsi), "ship");
    }

    #[test]
    fn vdm_field_errors() {
        fn p(l: &str) -> Result<Vdm<'_>, VdmErr> {
            parse_vdm(&split(l).unwrap())
        }
        assert_eq!(p("!AIVDM,1,1,,A,1,0").unwrap().chan, Chan::A);
        assert_eq!(p("!AIVDM,1,1,,1,1,0").unwrap().chan, Chan::A);
        assert_eq!(p("!AIVDM,1,1,,2,1,0").unwrap().chan, Chan::B);
        assert_eq!(p("!AIVDM,1,1,,,1,0").unwrap().chan, Chan::Unknown);
        assert!(p("!AIVDO,1,1,,,1,0").unwrap().own);
        assert_eq!(p("!AIVDM,1,1,,A,1"), Err(VdmErr::Fields));
        assert_eq!(p("!AIVDM,0,1,,A,1,0"), Err(VdmErr::Count));
        assert_eq!(p("!AIVDM,10,1,,A,1,0"), Err(VdmErr::Count));
        assert_eq!(p("!AIVDM,2,3,1,A,1,0"), Err(VdmErr::Num));
        assert_eq!(p("!AIVDM,2,0,1,A,1,0"), Err(VdmErr::Num));
        assert_eq!(p("!AIVDM,2,1,x,A,1,0"), Err(VdmErr::Seq));
        assert_eq!(p("!AIVDM,1,1,,A,1,6"), Err(VdmErr::Fill));
        assert_eq!(p("!AIVDM,1,1,,A,1,"), Err(VdmErr::Fill));
        assert_eq!(p("!AIVDM,2,1,3,A,1,").unwrap().fill, 0);
        assert_eq!(p("!AIVDM,1,1,,A,,0"), Err(VdmErr::EmptyPayload));
    }

    #[test]
    fn armor() {
        assert_eq!(
            dearmor("0W`w", 0).unwrap().uint(0, 24),
            Some(0b000000_100111_101000_111111)
        );
        assert_eq!(dearmor("X", 0), Err(ArmorErr::BadChar('X')));
        assert_eq!(dearmor("x", 0), Err(ArmorErr::BadChar('x')));
        assert_eq!(dearmor("", 1), Err(ArmorErr::Fill));
        let b = dearmor("w", 2).unwrap();
        assert_eq!((b.len, b.uint(0, 4), b.uint(0, 5)), (4, Some(15), None));
        assert_eq!(b.uint(0, 33), None);
    }

    #[test]
    fn header_checks() {
        // 6 chars = 36 bits < 38
        assert_eq!(
            header(&dearmor("100000", 0).unwrap()),
            Err(HeaderErr::Short(36))
        );
        // type 0 and type 28 (0b011100 -> 'L')
        assert_eq!(
            header(&dearmor("0000000", 0).unwrap()),
            Err(HeaderErr::Type(0))
        );
        assert_eq!(
            header(&dearmor("L000000", 0).unwrap()),
            Err(HeaderErr::Type(28))
        );
        // a type 1 with only 42 bits is short, not an error
        let h = header(&dearmor("1000000", 0).unwrap()).unwrap();
        assert_eq!((h.mtype, h.len), (1, LenCheck::Short));
        for t in 1..=27 {
            let (lo, hi) = len_range(t);
            assert!(lo <= hi && hi <= 1064, "{t}");
        }
    }

    #[test]
    fn mmsi_kinds() {
        assert_eq!(mmsi_kind(2_442_000), "coast");
        assert_eq!(mmsi_kind(24_420_000), "group");
        assert_eq!(mmsi_kind(111_440_001), "SAR air");
        assert_eq!(mmsi_kind(992_351_000), "AtoN");
        assert_eq!(mmsi_kind(970_012_345), "SART");
        assert_eq!(mmsi_kind(0), "invalid");
        assert_eq!(mmsi_kind(1_000_000_000), "other");
    }
}
