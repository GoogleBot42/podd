//! Structured view of the Frozen firmware's debug lines.
//!
//! Every few seconds the firmware prints its own control state as text
//! (`0x07` Message frames): PID terms per loop, pump drive and current, fan
//! duty and speed, solenoid currents. Nothing else on the wire shows whether
//! the TEC is actually being driven or the pumps are moving water, so the
//! telemetry recorder keeps the latest value of each.
//!
//! Known shapes (live Pod 4, 2026-10):
//!
//! ```text
//! FW: pid[left] 25.812500 0.090498 -0.003750 0.094248 0.000000
//! FW: pump[left] slow @ 6.030475V 0.169202A
//! FW: [top-fan] 0.00 @ 0 rpm
//! FW: [solenoid] solenoid_current @ 0.012435A
//! FW: [stats] avg:1ms max:12ms min:0ms
//! FW: temps 25.81 25.81 22.37 23.62
//! ```
//!
//! What the five `pid[...]` numbers mean is not documented; the second is
//! the sum of the third and fourth in every capture, which reads as
//! output = P + I. They are recorded raw.

use std::collections::BTreeMap;

use serde::Serialize;

/// One parsed firmware line.
#[derive(Debug, Clone, PartialEq)]
pub enum FwLine {
    Pid { name: String, terms: Vec<f32> },
    Pump(String, Pump),
    Fan(String, Fan),
    /// `[solenoid] solenoid_current @ …A`, `[float_valve] …` — amps.
    Current(String, f32),
    Stats(Stats),
    Temps(Vec<f32>),
    /// Anything this parser does not know (including `[priming]` and the
    /// water-tank lines, which `FrozenState` handles itself).
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pump {
    pub mode: String,
    pub v: f32,
    pub a: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fan {
    pub duty: f32,
    pub rpm: u32,
}

/// The firmware main loop's timing, ms.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stats {
    pub avg: u32,
    pub max: u32,
    pub min: u32,
}

/// Latest value of every firmware line seen since the last [`take`].
///
/// [`take`]: FwTelemetry::take
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FwTelemetry {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub pid: BTreeMap<String, Vec<f32>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub pump: BTreeMap<String, Pump>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fan: BTreeMap<String, Fan>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub amps: BTreeMap<String, f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temps: Option<Vec<f32>>,
    /// Lines folded in since the last take.
    pub lines: u32,
}

impl FwTelemetry {
    /// Fold one line in; returns whether it was a known shape.
    pub fn record(&mut self, msg: &str) -> bool {
        self.lines += 1;
        match parse(msg) {
            FwLine::Pid { name, terms } => {
                self.pid.insert(name, terms);
            }
            FwLine::Pump(side, p) => {
                self.pump.insert(side, p);
            }
            FwLine::Fan(name, f) => {
                self.fan.insert(name, f);
            }
            FwLine::Current(name, a) => {
                self.amps.insert(name, a);
            }
            FwLine::Stats(s) => self.stats = Some(s),
            FwLine::Temps(t) => self.temps = Some(t),
            FwLine::Other => return false,
        }
        true
    }

    /// Hand over what was collected and start empty, so a sample never
    /// repeats a value the firmware has stopped sending.
    pub fn take(&mut self) -> FwTelemetry {
        std::mem::take(self)
    }
}

pub fn parse(msg: &str) -> FwLine {
    let Some(body) = msg.strip_prefix("FW: ") else {
        return FwLine::Other;
    };
    parse_body(body).unwrap_or(FwLine::Other)
}

fn parse_body(body: &str) -> Option<FwLine> {
    if let Some(rest) = body.strip_prefix("pid[") {
        let (name, terms) = rest.split_once(']')?;
        let terms = floats(terms)?;
        return Some(FwLine::Pid { name: name.to_string(), terms });
    }
    if let Some(rest) = body.strip_prefix("pump[") {
        // left] slow @ 6.030475V 0.169202A
        let (side, rest) = rest.split_once(']')?;
        let (mode, rest) = rest.trim().split_once(" @ ")?;
        let mut it = rest.split_whitespace();
        let v = it.next()?.strip_suffix('V')?.parse().ok()?;
        let a = it.next()?.strip_suffix('A')?.parse().ok()?;
        let pump = Pump { mode: mode.to_string(), v, a };
        return Some(FwLine::Pump(side.to_string(), pump));
    }
    if let Some(rest) = body.strip_prefix("temps ") {
        return Some(FwLine::Temps(floats(rest)?));
    }
    let rest = body.strip_prefix('[')?;
    let (name, rest) = rest.split_once("] ")?;
    if name == "stats" {
        // avg:1ms max:12ms min:0ms
        let mut ms = [0u32; 3];
        for (slot, key) in ms.iter_mut().zip(["avg:", "max:", "min:"]) {
            let field = rest.split_whitespace().find_map(|f| f.strip_prefix(key))?;
            *slot = field.strip_suffix("ms")?.parse().ok()?;
        }
        return Some(FwLine::Stats(Stats { avg: ms[0], max: ms[1], min: ms[2] }));
    }
    let (lhs, rhs) = rest.split_once(" @ ")?;
    if let Some(rpm) = rhs.strip_suffix(" rpm") {
        // [top-fan] 0.00 @ 0 rpm
        let fan = Fan { duty: lhs.trim().parse().ok()?, rpm: rpm.trim().parse().ok()? };
        return Some(FwLine::Fan(name.to_string(), fan));
    }
    // [solenoid] solenoid_current @ 0.012435A
    let amps = rhs.trim().strip_suffix('A')?.parse().ok()?;
    Some(FwLine::Current(name.to_string(), amps))
}

fn floats(s: &str) -> Option<Vec<f32>> {
    let v: Option<Vec<f32>> = s.split_whitespace().map(|t| t.parse().ok()).collect();
    v.filter(|v| !v.is_empty())
}

/// A message with its numbers blanked, so repeats of one line shape can be
/// rate-limited together.
pub fn shape(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut in_num = false;
    for c in msg.chars() {
        if c.is_ascii_digit() || (in_num && c == '.') {
            if !in_num {
                out.push('#');
            }
            in_num = true;
        } else {
            in_num = false;
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_known_lines() {
        assert_eq!(
            parse("FW: pid[left] 25.812500 0.090498 -0.003750 0.094248 0.000000"),
            FwLine::Pid {
                name: "left".into(),
                terms: vec![25.8125, 0.090498, -0.00375, 0.094248, 0.0]
            }
        );
        assert_eq!(
            parse("FW: pump[right] off @ 0.000000V -0.000009A"),
            FwLine::Pump("right".into(), Pump { mode: "off".into(), v: 0.0, a: -0.000009 })
        );
        assert_eq!(
            parse("FW: pump[left] slow @ 6.030475V 0.169202A"),
            FwLine::Pump("left".into(), Pump { mode: "slow".into(), v: 6.030475, a: 0.169202 })
        );
        assert_eq!(
            parse("FW: [bottom-fan] 0.20 @ 330 rpm"),
            FwLine::Fan("bottom-fan".into(), Fan { duty: 0.2, rpm: 330 })
        );
        assert_eq!(
            parse("FW: [float_valve] solenoid_current @ 0.002229A"),
            FwLine::Current("float_valve".into(), 0.002229)
        );
        assert_eq!(
            parse("FW: [stats] avg:1ms max:12ms min:0ms"),
            FwLine::Stats(Stats { avg: 1, max: 12, min: 0 })
        );
        assert_eq!(
            parse("FW: temps 25.81 25.81 22.37 23.62"),
            FwLine::Temps(vec![25.81, 25.81, 22.37, 23.62])
        );
    }

    #[test]
    fn unknown_and_malformed_lines_are_other() {
        for msg in [
            "FW: [priming] start",
            "FW: water full -> empty",
            "FW: pid[left] nan-ish",
            "FW: pump[left] slow",
            "FW: [top-fan] fast @ x rpm",
            "hello",
        ] {
            assert_eq!(parse(msg), FwLine::Other, "{msg}");
        }
    }

    #[test]
    fn take_drains() {
        let mut t = FwTelemetry::default();
        assert!(t.record("FW: [top-fan] 0.50 @ 900 rpm"));
        assert!(!t.record("FW: something new 12"));
        let got = t.take();
        assert_eq!(got.fan["top-fan"], Fan { duty: 0.5, rpm: 900 });
        assert_eq!(got.lines, 2);
        assert_eq!(t, FwTelemetry::default());
    }

    #[test]
    fn shape_blanks_numbers() {
        assert_eq!(shape("FW: err 12 at 3.25V"), "FW: err # at #V");
        assert_eq!(shape("FW: pid[left] 1.5 -2.0"), "FW: pid[left] # -#");
    }
}
