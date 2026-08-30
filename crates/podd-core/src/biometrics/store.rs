//! On-device persistence for computed vitals: an append-only JSONL file
//! (one record per line) under the data dir, pruned by age.
//!
//! JSONL over SQLite deliberately: records arrive ~once a minute per side,
//! reads are a UI page load over a bounded window, and a line-per-record
//! file survives partial writes (a torn last line is skipped on read).

use pod_proto::packet::BedSide;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Default retention for vitals history.
pub const RETENTION_DAYS: i64 = 90;

/// One computed vitals sample. `timestamp` is epoch seconds (start of the
/// measurement window). Units: `heart_rate` BPM, `hrv` ms (RMSSD),
/// `breathing_rate` breaths/min.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VitalsRecord {
    pub side: BedSide,
    pub timestamp: i64,
    pub heart_rate: i64,
    pub hrv: i64,
    pub breathing_rate: i64,
}

/// Append-only JSONL store for [`VitalsRecord`]s.
pub struct VitalsStore {
    path: PathBuf,
    // Serializes append+prune; reads take it too so a prune rewrite can't
    // race a reader over the same file.
    lock: Mutex<()>,
}

impl VitalsStore {
    /// Open (creating parent dirs). Does not prune — call [`Self::prune`]
    /// at startup with a "now" the caller trusts (post-NTP).
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(VitalsStore {
            path,
            lock: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, record: &VitalsRecord) -> std::io::Result<()> {
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
    ) -> std::io::Result<Vec<VitalsRecord>> {
        let _guard = self.lock.lock().unwrap();
        let f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for line in BufReader::new(f).lines() {
            let line = line?;
            let Ok(rec) = serde_json::from_str::<VitalsRecord>(&line) else {
                continue;
            };
            if start.is_some_and(|s| rec.timestamp < s) || end.is_some_and(|e| rec.timestamp > e) {
                continue;
            }
            if side.is_some_and(|s| s != rec.side) {
                continue;
            }
            out.push(rec);
        }
        Ok(out)
    }

    /// Drop records older than `retention_days` before `now_unix` by
    /// atomically rewriting the file (write temp + rename).
    pub fn prune(&self, now_unix: i64, retention_days: i64) -> std::io::Result<()> {
        let cutoff = now_unix - retention_days * 24 * 3600;
        let _guard = self.lock.lock().unwrap();
        let f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut kept = String::new();
        let mut dropped = 0usize;
        for line in BufReader::new(f).lines() {
            let line = line?;
            match serde_json::from_str::<VitalsRecord>(&line) {
                Ok(rec) if rec.timestamp < cutoff => dropped += 1,
                Ok(_) => {
                    kept.push_str(&line);
                    kept.push('\n');
                }
                // torn/foreign lines are dropped on prune
                Err(_) => dropped += 1,
            }
        }
        if dropped == 0 {
            return Ok(());
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, kept)?;
        std::fs::rename(&tmp, &self.path)?;
        log::info!("vitals store: pruned {dropped} records older than {retention_days}d");
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
}
