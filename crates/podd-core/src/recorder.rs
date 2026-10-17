//! Thermal flight recorder: one JSON line per sample or event, on the data
//! partition, so a bad night can be reconstructed afterwards.
//!
//! The journal says what podd *decided*; it never had the inputs. When the
//! bed misbehaved (the 2026-10-05 cold night, the 2026-10-15 freeze-guard
//! trips) the only numbers left were a 5-minute water/cover line — nothing
//! on pump current, TEC drive, fans, or what the freeze guard was thinking.
//! The recorder keeps all of that at 10 s resolution.
//!
//! Files: `<data dir>/telemetry/YYYY-MM-DD.jsonl` (UTC day), appended as
//! lines arrive. On the first write of a new day every older `.jsonl` is
//! gzipped to `.jsonl.gz`, and the oldest `.gz` files are deleted until the
//! directory fits [`BUDGET`]. A day's raw file stops growing at
//! [`DAY_CAP`] (a flood must not fill `/data`, which also holds the config).
//!
//! Lines have `ts` (UTC, seconds) and `t` (epoch seconds, ms precision) and
//! a `kind`: `"sample"` (built by the Frozen manager, see
//! `frozen::sample`), `"log"` (podd's own log lines at INFO and above, via
//! [`TeeLogger`]), or `"start"`.
//!
//! The writer runs on its own thread and producers only ever `try_send`: a
//! slow or failing SD card drops telemetry, never stalls the control loops.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Directory under the data dir.
pub const DIR: &str = "telemetry";
/// Everything in the directory, compressed history plus today's raw file.
/// At ~5 MB raw / ~0.5 MB gzipped a day this is weeks of history; `/data`
/// is 230 MB with ~65 MB free on the live unit.
const BUDGET: u64 = 24 * 1024 * 1024;
/// One day's raw file stops growing here.
const DAY_CAP: u64 = 16 * 1024 * 1024;
/// Lines buffered between producers and the writer thread.
const QUEUE: usize = 1024;
/// Log lines teed into the recorder per minute; the rest are counted.
const LOGS_PER_MIN: u32 = 120;

/// Producer handle. Cheap to clone; a default one records nothing.
#[derive(Clone, Debug, Default)]
pub struct Recorder {
    tx: Option<mpsc::Sender<String>>,
}

impl Recorder {
    /// Start the writer thread on `dir` and route teed log lines to it.
    pub fn start(dir: PathBuf) -> Recorder {
        let (tx, rx) = mpsc::channel(QUEUE);
        let spawned = std::thread::Builder::new()
            .name("recorder".into())
            .spawn(move || DayFiles::new(dir).run(rx));
        if let Err(e) = spawned {
            log::error!("telemetry recorder thread failed to start: {e}");
            return Recorder::default();
        }
        let _ = LOG_TX.set(tx.clone());
        Recorder { tx: Some(tx) }
    }

    /// Queue one record of `kind`; its fields are merged into the line after
    /// the timestamp. Dropped (silently) if the writer is behind.
    pub fn record(&self, kind: &str, fields: impl Serialize) {
        let Some(tx) = &self.tx else { return };
        if let Some(line) = line(kind, fields) {
            let _ = tx.try_send(line);
        }
    }
}

fn line(kind: &str, fields: impl Serialize) -> Option<String> {
    let now = jiff::Timestamp::now();
    let mut v = json!({
        "ts": now.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "t": now.as_millisecond() as f64 / 1000.0,
        "kind": kind,
    });
    if let (Value::Object(head), Ok(Value::Object(rest))) = (&mut v, serde_json::to_value(fields)) {
        head.extend(rest);
    }
    serde_json::to_string(&v).ok()
}

/// The UTC day a line belongs in, `YYYY-MM-DD`.
fn today() -> String {
    jiff::Timestamp::now().strftime("%Y-%m-%d").to_string()
}

struct DayFiles {
    dir: PathBuf,
    day: String,
    file: Option<BufWriter<File>>,
    size: u64,
    /// Set after a write error or the day cap, so the journal gets one line,
    /// not one per sample.
    complained: bool,
}

impl DayFiles {
    fn new(dir: PathBuf) -> Self {
        DayFiles { dir, day: String::new(), file: None, size: 0, complained: false }
    }

    fn run(mut self, mut rx: mpsc::Receiver<String>) {
        while let Some(line) = rx.blocking_recv() {
            self.write(&line);
        }
    }

    fn write(&mut self, line: &str) {
        let day = today();
        if day != self.day || self.file.is_none() {
            if day != self.day {
                self.complained = false;
            }
            self.day = day;
            if let Err(e) = self.open() {
                self.complain(format_args!("cannot open {}: {e}", self.path().display()));
                return;
            }
        }
        if self.size + line.len() as u64 + 1 > DAY_CAP {
            self.complain(format_args!("{} reached {DAY_CAP} bytes; dropping the rest of the day", self.path().display()));
            return;
        }
        let Some(f) = &mut self.file else { return };
        let res = f.write_all(line.as_bytes()).and_then(|_| f.write_all(b"\n")).and_then(|_| f.flush());
        match res {
            Ok(()) => self.size += line.len() as u64 + 1,
            Err(e) => {
                self.complain(format_args!("write to {} failed: {e}", self.path().display()));
                // reopen on the next line
                self.file = None;
            }
        }
    }

    fn complain(&mut self, msg: std::fmt::Arguments) {
        if !self.complained {
            self.complained = true;
            log::error!("telemetry recorder: {msg}");
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(format!("{}.jsonl", self.day))
    }

    /// Open today's file for append, after compressing older days and
    /// pruning to the budget.
    fn open(&mut self) -> io::Result<()> {
        self.file = None;
        fs::create_dir_all(&self.dir)?;
        let path = self.path();
        compress_old(&self.dir, &path);
        prune(&self.dir, BUDGET);
        let f = OpenOptions::new().create(true).append(true).open(&path)?;
        self.size = f.metadata()?.len();
        self.file = Some(BufWriter::new(f));
        Ok(())
    }
}

/// Gzip every `.jsonl` in `dir` other than `current`.
fn compress_old(dir: &Path, current: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for path in entries.flatten().map(|e| e.path()) {
        if path == current || path.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        if let Err(e) = gzip(&path) {
            log::warn!("telemetry recorder: compressing {} failed: {e}", path.display());
        }
    }
}

fn gzip(path: &Path) -> io::Result<()> {
    let gz = path.with_extension("jsonl.gz");
    let tmp = path.with_extension("jsonl.gz.tmp");
    {
        let mut src = File::open(path)?;
        let mut enc = flate2::write::GzEncoder::new(File::create(&tmp)?, flate2::Compression::default());
        io::copy(&mut src, &mut enc)?;
        enc.finish()?.sync_all()?;
    }
    fs::rename(&tmp, &gz)?;
    fs::remove_file(path)
}

/// Delete the oldest `.jsonl.gz` files (names sort by date) until the
/// directory's files total at most `budget` bytes. Raw files are never
/// deleted: only today's should exist, and it is capped on its own.
fn prune(dir: &Path, budget: u64) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut files: Vec<(PathBuf, u64)> = entries
        .flatten()
        .filter_map(|e| Some((e.path(), e.metadata().ok().filter(|m| m.is_file())?.len())))
        .collect();
    files.sort();
    let mut total: u64 = files.iter().map(|(_, n)| n).sum();
    for (path, n) in files {
        if total <= budget {
            break;
        }
        if path.to_string_lossy().ends_with(".jsonl.gz") && fs::remove_file(&path).is_ok() {
            total -= n;
        }
    }
}

static LOG_TX: OnceLock<mpsc::Sender<String>> = OnceLock::new();

/// Wraps the process logger and copies podd's own INFO+ lines (and every
/// crate's WARN+) into the recorder, so samples and decisions sit in one
/// time-ordered file. Rate-limited to [`LOGS_PER_MIN`]; the excess is
/// counted in a `log` line once the minute is over.
pub struct TeeLogger<L> {
    inner: L,
    /// `(minute, teed this minute, dropped this minute)`
    window: Mutex<(i64, u32, u32)>,
}

impl<L: log::Log> TeeLogger<L> {
    pub fn new(inner: L) -> Self {
        TeeLogger { inner, window: Mutex::new((0, 0, 0)) }
    }

    fn wanted(r: &log::Record) -> bool {
        let t = r.target();
        if t.starts_with("podd_core::recorder") {
            return false;
        }
        let ours = ["podd", "api", "pod_"].iter().any(|p| t.starts_with(p));
        r.level() <= log::Level::Warn || (ours && r.level() <= log::Level::Info)
    }
}

impl<L: log::Log> log::Log for TeeLogger<L> {
    fn enabled(&self, m: &log::Metadata) -> bool {
        self.inner.enabled(m)
    }

    fn log(&self, r: &log::Record) {
        let enabled = self.inner.enabled(r.metadata());
        if enabled {
            self.inner.log(r);
        }
        let Some(tx) = LOG_TX.get() else { return };
        if !enabled || !Self::wanted(r) {
            return;
        }
        let minute = jiff::Timestamp::now().as_second() / 60;
        let dropped_before = {
            let Ok(mut w) = self.window.lock() else { return };
            let mut dropped_before = 0;
            if w.0 != minute {
                dropped_before = w.2;
                *w = (minute, 0, 0);
            }
            if w.1 >= LOGS_PER_MIN {
                w.2 += 1;
                return;
            }
            w.1 += 1;
            dropped_before
        };
        if dropped_before > 0
            && let Some(l) = line("log", json!({"lvl": "WARN", "tgt": "podd_core::recorder",
                "msg": format!("{dropped_before} log lines over the {LOGS_PER_MIN}/min limit were not recorded")}))
        {
            let _ = tx.try_send(l);
        }
        let fields = json!({"lvl": r.level().as_str(), "tgt": r.target(), "msg": r.args().to_string()});
        if let Some(l) = line("log", fields) {
            let _ = tx.try_send(l);
        }
    }

    fn flush(&self) {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("podd-recorder-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn line_has_timestamp_kind_and_fields() {
        let l = line("sample", json!({"a": 1, "b": "x"})).unwrap();
        let v: Value = serde_json::from_str(&l).unwrap();
        assert_eq!(v["kind"], "sample");
        assert_eq!(v["a"], 1);
        assert!(v["ts"].as_str().unwrap().ends_with('Z'));
        assert!(v["t"].as_f64().unwrap() > 1.7e9);
    }

    #[test]
    fn writes_compresses_old_days_and_prunes() {
        let dir = tmpdir("rotate");
        fs::write(dir.join("2026-01-01.jsonl"), "{\"old\":1}\n".repeat(1000)).unwrap();
        fs::write(dir.join("2025-12-30.jsonl.gz"), vec![0u8; 4000]).unwrap();
        fs::write(dir.join("2025-12-31.jsonl.gz"), vec![0u8; 4000]).unwrap();
        let mut w = DayFiles::new(dir.clone());
        w.write("{\"x\":1}");
        w.write("{\"x\":2}");
        let written = fs::read_to_string(dir.join(format!("{}.jsonl", today()))).unwrap();
        assert_eq!(written, "{\"x\":1}\n{\"x\":2}\n");
        // yesterday's raw file is now gzipped, and decompresses to itself
        assert!(!dir.join("2026-01-01.jsonl").exists());
        let gz = File::open(dir.join("2026-01-01.jsonl.gz")).unwrap();
        let mut out = String::new();
        io::Read::read_to_string(&mut flate2::read::GzDecoder::new(gz), &mut out).unwrap();
        assert_eq!(out, "{\"old\":1}\n".repeat(1000));
        // under budget: nothing pruned
        assert!(dir.join("2025-12-30.jsonl.gz").exists());
        // a tight budget removes the oldest first and never the raw file
        prune(&dir, 5000);
        assert!(!dir.join("2025-12-30.jsonl.gz").exists());
        assert!(dir.join(format!("{}.jsonl", today())).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_full_day_stops_growing() {
        let dir = tmpdir("cap");
        let mut w = DayFiles::new(dir.clone());
        w.write("{}");
        w.size = DAY_CAP - 2;
        w.write("{\"too\":\"big\"}");
        let written = fs::read_to_string(dir.join(format!("{}.jsonl", today()))).unwrap();
        assert_eq!(written, "{}\n");
        fs::remove_dir_all(&dir).unwrap();
    }
}
