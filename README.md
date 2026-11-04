# aisnomon

A real-time AIS and NMEA 0183 reception monitor.

aisnomon is a diagnostic tool for monitoring AIS reception quality, NMEA data
integrity and message statistics. It is designed for AIS receiver operators
who want to understand and monitor their reception environment.

**A tool for observing the AIS reception environment, not for viewing vessels
on a map.** It does not decode positions and has no map. It answers:

- **Is the receiver hearing anything, and how much?** Messages per second,
  live and as a timeline from seconds to a day.
- **Are both channels working?** Messages on AIS 1 (A, 161.975 MHz) and
  AIS 2 (B, 162.025 MHz), side by side.
- **Is the NMEA output clean?** Checksum errors, non-NMEA noise, malformed
  VDM fields, bad 6-bit armoring, wrong payload lengths.
- **Do multi-sentence messages arrive whole?** Lost, duplicate, out-of-order
  and timed-out fragments.
- **Who is heard, how often?** Unique MMSIs in a recent window and in total,
  with per-MMSI counts, channel split, mean interval and message types.

Part of the same family as [gnomon](https://github.com/tanyorg/gnomon)
(GNSS/NMEA monitor) and metronomon (PPS monitor), and built on the same
terminal and serial code. No external crates (std only). Read-only: it never
writes to a receiver.

```
aisnomon 0.1.0  ais-rx  tcp://127.0.0.1:10115  connected 127.0.0.1:  2026-11-04 05:25:49Z
┌ SUMMARY ─────────────────────────────────────────────────────────────────────────────────────┐
│ Messages   2074               AIS A      1037     50.0%     Valid      2092     99.3%        │
│ Rate 10s   103.3/s            AIS B      1037     50.0%     CK error   15       0.7%         │
│ Rate 1m    103.7/s            No chan    0        0.0%      Not NMEA   0                     │
│ Last msg   0s ago             MMSI 10m   34                 Incomplete 6                     │
│ Uptime     20s                MMSI all   34                 Decode err 0                     │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
┌ RECEPTION TIMELINE  msg/s, 1s/column  [t] ───────────────────────────────────────────────────┐
│109.0/s                                                                  ▇▄▆▆▅▇▇▆▅▅█▄▅▄▇▄█▅▄▅ │
│                                                                         ████████████████████ │
│                                                                         ████████████████████ │
│                                                                         ████████████████████ │
│                                                                         ████████████████████ │
│      0                                                                  ████████████████████ │
│        -1m20s    -1m10s       -1m      -50s      -40s      -30s      -20s      -10s      now │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
┌ NMEA RECEIVED  [p] pause ────────────────────────────────────────────────────────────────────┐
│ 05:25:49.119 B     T3 246859077         !AIVDM,1,1,,B,33cK5AGoJd=@h@SbJKp<1C7aq@@;,0*38      │
│ 05:25:49.143 A     T3 431996382         !AIVDM,1,1,,A,36KvuocMeo4sHg=BmLdqERMP6pj5,0*10      │
│ 05:25:49.144 B     T1 431411439         !AIVDM,1,1,,B,16KK@so>k5t8BMnpu?0r8:ToFhu8,0*11      │
│ 05:25:49.152 A     T1 244228807         !AIVDM,1,1,,A,13`rRip>oIN9;Jo>`J1fTePQI2jt,0*1A      │
│ 05:25:49.160 B     T3 257155766         !AIVDM,1,1,,B,33m?ReRje>FoQI>6S?kS`1chJwW>,0*40      │
│ 05:25:49.176 A     T3 265608064         !AIVDM,1,1,,A,33uCKP5G0Bo3`rGmHN<=NIIPjrbW,0*5B      │
│ 05:25:49.187 B     T1 218638539         !AIVDM,1,1,,B,13@P`jnM0unftU<EaBKJR`immVDB,0*22      │
│ 05:25:49.197 A     T1 265730901         !AIVDM,1,1,,A,13uJsEFdo5klUn8rHQTH;6KsKM=V,0*7E      │
│ 05:25:49.209 B     T3 431098702         !AIVDM,1,1,,B,36K8;CQh6a;9GidE7eGMn@G9E6Vc,0*06      │
│ 05:25:49.214 A     T1 563129815         !AIVDM,1,1,,A,18I2emr2tRP:D;RpLWWqW@3s>J6g,0*42      │
│ 05:25:49.224 B     T1 245598646         !AIVDM,1,1,,B,13b>9eP7=MiEfLclV8F5>aswA1sK,0*37      │
│ 05:25:49.236 A     T1 257560559         !AIVDM,1,1,,A,13m`?sl=o5;SB?rR=fV9S@rmqfa2,0*27      │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
1 main 2 types 3 MMSI 4 raw  p pause  j/k scroll  t scale  c clear  q quit
```

## Install

```sh
cargo install --path .
```

Rust 1.80 or later. Runs on Linux, macOS, FreeBSD and OpenBSD (the serial
termios layer covers all four).

## Usage

```sh
aisnomon /dev/ttyUSB0                 # serial; probes 38400, 4800, 9600, ...
aisnomon -s 38400 /dev/cuaU0          # serial at a fixed speed
aisnomon 192.0.2.10:10110             # raw NMEA over TCP (client)
aisnomon udp://:10110                 # listen for UDP on 0.0.0.0:10110
aisnomon udp://[::]:10110             # listen on IPv6 (and IPv4 where the OS
                                      #   allows dual-stack sockets)
aisnomon udp://239.192.0.4:60004      # join a multicast group (IEC 61162-450)
aisnomon -l ais.log 192.0.2.10:10110  # also append every line to ais.log
aisnomon -f ais.log                   # replay a capture
nc 192.0.2.10 10110 | aisnomon --snapshot 60 -f -   # one screen, no TUI
```

`--snapshot SECS` runs without a terminal and prints one screen, for cron,
scripts or sharing; `--view 1-4` picks which screen.

### Keys

| Key | Action |
|---|---|
| `1` `2` `3` `4` | main view, channels/types, MMSI table, full-height raw log |
| `p`, space | pause / resume: every view shows the same frozen moment while reception and counting go on; the footer shows when it was taken |
| `j` `k`, arrows | scroll the NMEA log (views 1 and 4) or the MMSI table (view 3) one line; scrolling pauses |
| `b` `f`, PgUp PgDn | scroll one page |
| `t` | timeline scale: 1 s, 10 s, 1 min, 10 min, 1 h per column |
| `s` | MMSI table: sort by last heard or by message count |
| `c` | clear all statistics (also ends a pause) |
| `q`, `^C` | quit |

## Screens

**SUMMARY** — messages received, rate over the last 10 s and 1 min, time
since the last message, messages per channel, MMSIs heard in the window
(`-w`, default 10 min) and in total, valid sentences, checksum errors,
non-NMEA lines, incomplete multi-part messages and decode errors.

**RECEPTION TIMELINE** — mean messages per second per column. The newest
column covers the complete seconds of the current interval.

**NMEA RECEIVED** — every received line with its receive time (UTC), the
channel, the fragment number and the result: `T1 244123456` (type and MMSI),
`frag`, `CK 5D!=5C`, `not NMEA`, `bad fill bits`, `T5 ... short 418b`, ...

See [docs/aisnomon.md](docs/aisnomon.md) for every counter's definition.

### Interpretation caveat

AIS reporting intervals are not constant. They depend on vessel speed, course
changes, navigational status, class (A/B) and message type, and the number of
vessels in range changes through the day. A lower message rate or a longer
interval is not by itself a reception fault. Look for it together with
channel imbalance, rising error counts, or base stations (type 4, reported
every 10 s) and AtoNs dropping out.

## Testing without a receiver

`tools/mockais.py` generates class A/B, base station and AtoN traffic:

```sh
tools/mockais.py tcp 10110 &                    # then: aisnomon localhost:10110
tools/mockais.py --speedup 10 --ck-errors 0.02 --drop-frag 0.1 udp 127.0.0.1:10110
tools/mockais.py --fade 120 pty                 # prints a pty path for aisnomon
tools/mockais.py --tag --duration 600 stdout > capture.nmea
```

## Limitations

- Only the common header (type, repeat, MMSI) is decoded, plus a payload
  length check per message type. Positions, names and binary application
  data are deliberately not decoded.
- gpsd is not supported as an input (it re-encodes AIS as JSON). Use the raw
  NMEA feed of the receiver or a TCP/UDP forwarder.
- `std::net` cannot set `SO_REUSEADDR`/`SO_REUSEPORT`, so a UDP port already
  bound by another program cannot be shared.
- `--log` records lines without receive times. Captures that carry tag block
  `c:` times replay paced; others replay as fast as they are read.
- Own-station `!AIVDO` messages are counted separately and excluded from
  reception statistics, since they were not received over the air.

## License

BSD 2-Clause. See [LICENSE](LICENSE).
