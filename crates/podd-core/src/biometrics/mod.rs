//! Biometrics: vitals (heart rate, HRV, breathing rate) computed from the
//! sensor MCU's piezo stream, and their on-device history store.
//!
//! Ported from free-sleep's Python `biometrics/` pipeline (MIT); see #12.

pub mod dsp;
pub mod heart;
pub mod processor;
pub mod store;

pub use store::{VitalsRecord, VitalsStore, RETENTION_DAYS};
