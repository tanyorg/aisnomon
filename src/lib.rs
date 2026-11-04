//! aisnomon: real-time AIS and NMEA 0183 reception monitor.
//!
//! The library is an implementation detail of the binary; its API is not
//! stable. `term`, `tty` and parts of `serial`, `source` and `nmea` come
//! from gnomon (https://github.com/tanyorg/gnomon).

pub mod ais;
pub mod app;
pub mod nmea;
pub mod serial;
pub mod source;
pub mod target;
pub mod term;
pub mod tty;
pub mod ui;
