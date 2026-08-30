//! On-device persistence for computed biometrics: append-only JSONL files
//! (one record per line) under the data dir, pruned by age.
//!
//! JSONL over SQLite deliberately: records arrive at most once a minute per
//! side, reads are a UI page load over a bounded window, and a line-per-record
//! file survives partial writes (a torn last line is skipped on read).
//!
//! One generic store type backs all three histories (vitals, sleep records,
//! movement); a record type only has to say where it sits in time and which
//! side it belongs to.

use pod_proto::packet::BedSide;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Default retention for biometrics history.
pub const RETENTION_DAYS: i64 = 90;

/// A record a [`JsonlStore`] can place in time and on a side.
pub trait StoredRecord: Serialize + DeserializeOwned {
    /// Human-readable store name, used in log lines only.
    const LABEL: &'static str;
    /// Epoch seconds this record is anchored at (what queries and pruning
    /// filter on).
    fn timestamp(&self) -> i64;
    fn side(&self) -> BedSide;
}

/// One computed vitals sample. `timestamp` is epoch seconds (start of the
/// measurement window). Units: `heart_rate` BPM, `hrv` ms (SDNN),
/// `breathing_rate` breaths/min.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VitalsRecord {
    pub side: BedSide,
    pub timestamp: i64,
    pub heart_rate: i64,
    pub hrv: i64,
    pub breathing_rate: i64,
}

impl StoredRecord for VitalsRecord {
    const LABEL: &'static str = "vitals";
    fn timestamp(&self) -> i64 {
        self.timestamp
    }
    fn side(&self) -> BedSide {
        self.side
    }
}

/// Append-only JSONL store for one record type.
pub struct JsonlStore<T> {
    path: PathBuf,
    // Serializes append+rewrite; reads take it too so a prune rewrite can't
    // race a reader over the same file.
    lock: Mutex<()>,
    _record: PhantomData<fn() -> T>,
}

/// History of computed vitals (HR / HRV / breathing rate).
pub type VitalsStore = JsonlStore<VitalsRecord>;

impl<T: StoredRecord> JsonlStore<T> {
    /// Open (creating parent dirs). Does not prune — call [`Self::prune`]
    /// at startup with a "now" the caller trusts (post-NTP).
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(JsonlStore {
            path,
            lock: Mutex::new(()),
            _record: PhantomData,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, record: &T) -> std::io::Result<()> {
        let line = serde_json::to_string(record)?;
        let _guard = self.lock.lock().unwrap();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")?;
        Ok(())
    }

    /// All records in `[start, end]` (inclusive, either side optional) for
    /// `side` (both sides when `None`), in file (= chronological) order.
    /// Unparseable lines (e.g. a torn tail from a power cut) are skipped.
    pub fn query(
        &self,
        start: Option<i64>,
        end: Option<i64>,
        side: Option<BedSide>,
    ) -> std::io::Result<Vec<T>> {
        let _guard = self.lock.lock().unwrap();
        let f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for line in BufReader::new(f).lines() {
            let line = line?;
            let Ok(rec) = serde_json::from_str::<T>(&line) else {
                continue;
            };
            if start.is_some_and(|s| rec.timestamp() < s)
                || end.is_some_and(|e| rec.timestamp() > e)
            {
                continue;
            }
            if side.is_some_and(|s| s != rec.side()) {
                continue;
            }
            out.push(rec);
        }
        Ok(out)
    }

    /// Pass every stored record through `f` and atomically rewrite the file
    /// (write temp + rename) with the results; `None` drops a record. Torn or
    /// foreign lines are dropped. Returns how many records were dropped or
    /// changed; the file is left untouched when that count is zero.
    pub fn rewrite(&self, mut f: impl FnMut(T) -> Option<T>) -> std::io::Result<usize> {
        let _guard = self.lock.lock().unwrap();
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut kept = String::new();
        let mut touched = 0usize;
        for line in BufReader::new(file).lines() {
            let line = line?;
            let Ok(rec) = serde_json::from_str::<T>(&line) else {
                // torn/foreign lines are dropped on any rewrite
                touched += 1;
                continue;
            };
            match f(rec) {
                Some(rec) => {
                    let out = serde_json::to_string(&rec)?;
                    if out != line {
                        touched += 1;
                    }
                    kept.push_str(&out);
                    kept.push('\n');
                }
                None => touched += 1,
            }
        }
        if touched == 0 {
            return Ok(0);
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, kept)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(touched)
    }

    /// Drop records older than `retention_days` before `now_unix`.
    pub fn prune(&self, now_unix: i64, retention_days: i64) -> std::io::Result<()> {
        let cutoff = now_unix - retention_days * 24 * 3600;
        let dropped = self.rewrite(|rec| (rec.timestamp() >= cutoff).then_some(rec))?;
        if dropped > 0 {
            log::info!(
                "{} store: pruned {dropped} records older than {retention_days}d",
                T::LABEL
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(side: BedSide, timestamp: i64) -> VitalsRecord {
        VitalsRecord {
            side,
            timestamp,
            heart_rate: 60,
            hrv: 45,
            breathing_rate: 14,
        }
    }

    fn tmp_store(tag: &str) -> VitalsStore {
        let dir = std::env::temp_dir().join(format!("podd-vitals-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        VitalsStore::open(dir.join("vitals.jsonl")).unwrap()
    }

    #[test]
    fn append_query_roundtrip_with_filters() {
        let store = tmp_store("roundtrip");
        store.append(&rec(BedSide::Left, 100)).unwrap();
        store.append(&rec(BedSide::Right, 200)).unwrap();
        store.append(&rec(BedSide::Left, 300)).unwrap();

        assert_eq!(store.query(None, None, None).unwrap().len(), 3);
        assert_eq!(
            store.query(None, None, Some(BedSide::Left)).unwrap(),
            vec![rec(BedSide::Left, 100), rec(BedSide::Left, 300)]
        );
        assert_eq!(
            store.query(Some(150), Some(250), None).unwrap(),
            vec![rec(BedSide::Right, 200)]
        );
        // inclusive bounds
        assert_eq!(store.query(Some(300), None, None).unwrap().len(), 1);
    }

    #[test]
    fn missing_file_reads_empty_and_torn_lines_are_skipped() {
        let store = tmp_store("torn");
        assert!(store.query(None, None, None).unwrap().is_empty());

        store.append(&rec(BedSide::Left, 100)).unwrap();
        // simulate a torn write (power cut mid-line)
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(store.path())
                .unwrap();
            write!(f, "{{\"side\":\"Left\",\"time").unwrap();
        }
        assert_eq!(store.query(None, None, None).unwrap().len(), 1);
    }

    #[test]
    fn prune_drops_old_records_atomically() {
        let store = tmp_store("prune");
        let day = 24 * 3600;
        store.append(&rec(BedSide::Left, 0)).unwrap();
        store.append(&rec(BedSide::Left, 89 * day)).unwrap();
        store.append(&rec(BedSide::Right, 90 * day)).unwrap();

        store.prune(90 * day, 90).unwrap(); // cutoff = 0 -> nothing < 0
        assert_eq!(store.query(None, None, None).unwrap().len(), 3);

        store.prune(91 * day, 90).unwrap(); // cutoff = 1 day -> drops t=0
        let left = store.query(None, None, None).unwrap();
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].timestamp, 89 * day);
    }

    #[test]
    fn rewrite_edits_and_drops_records() {
        let store = tmp_store("rewrite");
        store.append(&rec(BedSide::Left, 100)).unwrap();
        store.append(&rec(BedSide::Right, 200)).unwrap();
        store.append(&rec(BedSide::Left, 300)).unwrap();

        // no-op mapping touches nothing
        assert_eq!(store.rewrite(Some).unwrap(), 0);

        // drop one, edit another
        let touched = store
            .rewrite(|mut r| match r.timestamp {
                200 => None,
                300 => {
                    r.heart_rate = 99;
                    Some(r)
                }
                _ => Some(r),
            })
            .unwrap();
        assert_eq!(touched, 2);

        let left = store.query(None, None, None).unwrap();
        assert_eq!(left.len(), 2);
        assert_eq!(left[1].heart_rate, 99);
    }
}
