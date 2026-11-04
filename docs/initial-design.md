# Aisnomon — Project Concept & Initial Design

- Project name: **Aisnomon**
- Repository: `aisnomon`
- Category: AIS / NMEA 0183 Reception Monitor
- Status: Initial concept

## 1. Concept

Aisnomon is a real-time monitoring and diagnostic tool for people operating AIS receivers.

Its purpose is not to plot vessel positions on a map. It focuses on observing the receiver's operating condition, AIS reception, NMEA 0183 data integrity, and message statistics.

The project follows the same general philosophy as:

- `gnomon` — GNSS / NMEA monitoring
- `metronomon` — PPS monitoring
- `aisnomon` — AIS / NMEA reception monitoring

## 2. Intended users

The primary users are engineers and operators who install and operate AIS receivers.

Typical use cases:

- Checking AIS receiver operation
- Observing changes in antenna or reception conditions
- Comparing reception on AIS channels A and B
- Checking NMEA data integrity
- Investigating anomalies and changes in reception behavior

## 3. Project organization

Aisnomon should be an independent GitHub repository, separate from `gnomon` and `metronomon`, while remaining part of the same project family.

```text
tanyorg/
├── gnomon
├── metronomon
└── aisnomon
```

The repository name and executable name should both be `aisnomon`.

## 4. Main TUI layout

The initial interface is a three-section terminal user interface.

### 4.1 SUMMARY

Show the current overall reception state.

Candidate fields:

| Field | Meaning |
|---|---|
| Vessels | Unique MMSIs heard within a recent time window |
| Messages | Total messages received |
| Rate | Current message rate, in messages per second |
| AIS A | Messages received on AIS channel A |
| AIS B | Messages received on AIS channel B |
| Valid | Valid NMEA sentences |
| Checksum Error | Checksum error count and/or percentage |
| Uptime | Time since Aisnomon started |

Consider distinguishing the cumulative unique-MMSI count from the number of MMSIs heard within a recent time window.

### 4.2 RECEPTION TIMELINE

Plot the change in received-message volume over time.

For the first implementation, show messages per second (msg/s). Possible future additions:

- Separate time series for AIS channels A and B
- Message counts by AIS message type
- Longer-term reception trends

**Interpretation caveat:** AIS transmission intervals are not constant. They vary with vessel behavior and message type. A reduction in received messages or a longer interval must not automatically be classified as a reception fault.

### 4.3 NMEA RECEIVED

Show recently received NMEA sentences with timestamps.

Candidate information:

- Receive timestamp
- Raw NMEA sentence
- Checksum validation result
- AIS fragment information

AIS messages may be split across multiple NMEA sentences, so fragment reception and reassembly status should be observable.

The exact log interaction remains undecided: for example, a scrollable recent-lines view versus a continuously advancing display.

## 5. Input interfaces

Initial input candidates:

- Serial
- TCP
- UDP

Received NMEA data is parsed and used to update the statistics and live display.

## 6. Interaction and display modes

Keep the default screen uncluttered and focused on the receiver's current state.

Keyboard-selectable detail views are a possible extension:

- Summary
- Channel Statistics
- MMSI Statistics
- NMEA Raw Monitor

## 7. Design priorities

Aisnomon should be more than an AIS decoder. Its central purpose is to help operators observe the condition and behavior of an AIS reception environment.

Important observation areas:

- NMEA-level data integrity
- AIS message-level validity and decoding
- Message-rate trends
- Channel-specific reception
- Per-MMSI reception behavior
- Detection and investigation of anomalies or changes

## 8. Open design questions

The following items are not yet decided:

1. NMEA log behavior: continuously advancing output or a scrollable recent-lines view.
2. AIS decoding scope: support all message types initially or start with commonly used types.
3. Per-MMSI statistics: reception frequency, last-seen time, message types, and related details.
4. Statistics retention period and aggregation interval.
5. Command-line options and configuration.
6. Supported operating systems and receiver/input devices.

## 9. README introduction draft

> **Aisnomon**
>
> A real-time AIS and NMEA 0183 reception monitor.
>
> Aisnomon is a diagnostic tool for monitoring AIS reception quality, NMEA data integrity, and message statistics.
>
> It is designed for AIS receiver operators who want to understand and monitor their reception environment.

## 10. Core principle

**A tool for observing the AIS reception environment, not for viewing vessels on a map.**

This is the central design principle for Aisnomon. Keep the initial scope focused on reception monitoring and data quality; add further capabilities only when they support that purpose.
