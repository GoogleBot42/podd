//! Biometrics: vitals (heart rate, HRV, breathing rate) computed from the
//! sensor MCU's piezo stream, sleep sessions and movement from the piezo +
//! capacitance streams, and their on-device history stores.
//!
//! Ported from free-sleep's Python `biometrics/` pipeline (MIT); see #12
//! (vitals) and #141 (sleep detection / movement).

pub mod dsp;
pub mod heart;
pub mod processor;
pub mod sleep;
pub mod store;

pub use sleep::{MovementRecord, MovementStore, SideTracker, SleepRecord, SleepStore};
pub use store::{JsonlStore, RETENTION_DAYS, StoredRecord, VitalsRecord, VitalsStore};

use std::sync::Arc;

/// The three biometrics history stores, opened next to the config.
///
/// Every field is optional: a store that cannot be opened (read-only or full
/// `/data`) disables just that history rather than the daemon.
#[derive(Clone, Default)]
pub struct Stores {
    pub vitals: Option<Arc<VitalsStore>>,
    pub sleep: Option<Arc<SleepStore>>,
    pub movement: Option<Arc<MovementStore>>,
}

impl Stores {
    /// Open all three files in `dir`, pruning each to [`RETENTION_DAYS`].
    ///
    /// A pre-NTP clock (epoch ~0) makes the prune cutoff negative — a no-op,
    /// never an over-prune.
    pub fn open(dir: &std::path::Path) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Stores {
            vitals: open_one(dir.join("vitals.jsonl"), now),
            sleep: open_one(dir.join("sleep.jsonl"), now),
            movement: open_one(dir.join("movement.jsonl"), now),
        }
    }
}

fn open_one<T: StoredRecord>(path: std::path::PathBuf, now: i64) -> Option<Arc<JsonlStore<T>>> {
    match JsonlStore::<T>::open(path) {
        Ok(store) => {
            if let Err(e) = store.prune(now, RETENTION_DAYS) {
                log::warn!("{} store prune failed: {e}", T::LABEL);
            }
            Some(Arc::new(store))
        }
        Err(e) => {
            log::warn!(
                "{} store unavailable (that history is disabled): {e}",
                T::LABEL
            );
            None
        }
    }
}
