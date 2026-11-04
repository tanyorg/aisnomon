# aisnomon: counters and definitions

Every input line is classified exactly once, in this order:

1. An optional tag block (`\s:...,c:...*hh\`, IEC 61162-450 / NMEA 0183 v4)
   is removed and counted (**Tag blocks**). A bad tag block checksum is
   counted (**Tag block CK bad**) but does not reject the sentence.
2. A line that does not start with `!` or `$`, contains non-ASCII bytes or
   has an invalid address field is **Not NMEA**.
3. A sentence whose checksum does not match is **Checksum bad**; one without
   a checksum is **Checksum missing** (encapsulation sentences such as VDM
   require one). Neither is processed further. SUMMARY's **CK error** is
   their sum, as a share of all NMEA-shaped sentences.
4. Remaining sentences are **Valid**. Those that are not VDM/VDO are
   **Other sentences** (e.g. `$GPRMC` from a receiver with a GNSS, or
   proprietary status sentences).
5. VDM/VDO sentences whose encapsulation fields are malformed (fragment
   count/number, sequence id, fill bits, empty payload) are **Bad VDM
   fields**.
6. Fragments are reassembled (see below). A complete payload whose 6-bit
   armoring is invalid is an **Armor error**; one shorter than the 38-bit
   header or with message type 0 or > 27 is a **Header error**.
7. A message whose length is outside the range for its type is counted as
   **Length short** or **Length long** but is still a message.
8. A complete VDM message is counted in **Messages**, its channel, its type,
   the timeline and the MMSI table. VDO (own station) messages are counted
   only as **Own (VDO) msgs**.

SUMMARY's **Decode err** is Bad VDM fields + Armor errors + Header errors +
Length short + Length long.

## Channels

The channel field `A`/`B` (some receivers send `1`/`2`) is shown as AIS A
and AIS B. An empty or other value is **No chan**. A multi-part message is
attributed to the channel of its fragments.

## Fragment reassembly

Fragments are grouped by (talker+formatter, sequential message id, channel).

| Counter | Meaning |
|---|---|
| Multi-part msgs | complete messages that spanned more than one sentence |
| Incomplete groups | groups abandoned: a conflicting fragment started a new group under the same key, the group did not complete within 3 s, or the pending table (64 groups) overflowed |
| Fragment order | a fragment that did not follow its predecessor, including a group whose first fragment was lost |
| Duplicate frags | a fragment number (other than 1) received twice for a group |
| Pending frags | groups currently waiting for more fragments |

## Accepted payload lengths (bits)

Per ITU-R M.1371-5, with commonly seen deviations tolerated:

| Type | Bits | Type | Bits | Type | Bits |
|---|---|---|---|---|---|
| 1-3 | 168 | 10 | 72 | 19 | 312 |
| 4 | 168 | 11 | 168 | 20 | 72-160 |
| 5 | 420-424 | 12 | 72-1008 | 21 | 272-360 |
| 6 | 88-1008 | 13 | 72-168 | 22 | 168 |
| 7 | 72-168 | 14 | 40-1008 | 23 | 160 |
| 8 | 56-1008 | 15 | 88-160 | 24 | 160-168 |
| 9 | 168 | 16 | 96-144 | 25 | 40-168 |
|  |  | 17 | 80-816 | 26 | 60-1064 |
|  |  | 18 | 168 | 27 | 96-168 |

## Message types table (view 2)

| Column | Meaning |
|---|---|
| Msgs, % | received (VDM) messages of the type and their share of all messages |
| Len! | messages of the type whose length was outside the accepted range (short + long; VDO excluded) |
| Last | time since the type was last received |
| LastLen | bit length of the latest out-of-range message, with the accepted range in parentheses (only when the box is at least 48 columns wide) |

Counts that do not fit their column are abbreviated with decimal prefixes
(12.3M).

## Rates and windows

- **Rate 10s / Rate 1m**: complete messages over the last 10 or 60 complete
  seconds, divided by that time (less right after start or `c`).
- **Timeline**: per-second buckets kept for 24 h; each column is the mean
  msg/s of its interval.
- **MMSI 10m** (window set with `-w`): MMSIs with a message within the
  window. **MMSI all**: MMSIs since start; the table holds at most 20000
  entries, and when entries have been evicted the total is shown with `+`.
- **Interval** (MMSI view): (last - first) / (messages - 1).
- **Kind** (MMSI view): station category from the MMSI digits
  (ITU-R M.585): ship, coast (00MID), group (0MID), SAR air (111MID),
  handheld (8), SART (970), MOB (972), EPIRB (974), craft (98), AtoN (99).

## Inputs

| Input | Behaviour |
|---|---|
| Serial | 8N1, raw, CLOCAL, exclusive (TIOCEXCL). Without `-s`, 38400 (IEC 61162-2) is probed first, then 4800, 9600, 115200, 19200, 57600, 230400; one checksum-valid sentence within 5 s selects a speed. If none does, the port is used at 38400 so line noise stays visible. A quiet port is never reopened. |
| TCP | Client; read-only. Reconnects with 1-8 s back-off; a connection silent for 300 s is reopened (half-open detection). |
| UDP | Bound to the given address; a multicast address is joined on the default interface. Each datagram is self-contained; an IEC 61162-450 `UdPbC\0` header is removed. |
| Replay | `-f FILE` is paced by tag block `c:` times (gaps capped at 5 s); lines without one are delivered immediately. `-f -` reads stdin unpaced. |
