---
paths:
  - crates/podd-core/**
  - crates/podd/**
  - crates/pod-proto/**
---

# Actuation safety

- The sensor MCU is a zombie for about 60 seconds after any podd restart: it
  streams telemetry and answers Ping, but silently ignores alarm/actuation
  writes. Retry actuation-critical writes until firmware confirms — never
  fire-and-forget. (Deliberate duplication with the repo-root `CLAUDE.md`
  tripwire — keep both copies.)
- podd's LSP codec does no byte-stuffing. The Frozen MCU *does* (HDLC:
  `0x7D` escape, `7D 5E`/`7D 5D`; #214, sensor MCU unchecked). Until #214
  lands, a raw `0x7E` or `0x7D` anywhere in a frame gets it silently dropped,
  with no echo and no error. Setpoints must go through
  `FrozenTarget::delimiter_safe` (`crates/pod-proto/src/frozen/command.rs`)
  and alarm frames through `AlarmCommand::delimiter_safe`
  (`crates/pod-proto/src/sensor/command.rs`). Both nudge away from 0x7E only
  today. Compare the MCU's echo against the nudged value, not the requested
  one. Any NEW host→MCU command with variable payload bytes needs the same
  treatment.
- All MCU control writes must respect the `PODD_DRY_RUN` gate
  (`crates/podd/src/main.rs`); it defaults to dry-run (log, don't send).
- Alarms must never arm before NTP sync; there is no RTC battery.
- Cooling setpoints go through the freeze guard
  (`crates/podd-core/src/frozen/freeze.rs`: ramp, freeze detection, thaw) on
  every path that builds a `SetTargetTemperature` — the scheduler tick and
  live commands alike. A deep raw cooling target can ice the heat exchanger
  (issue #186). Compare the MCU echo against the guard's *returned* target.
  The surface loop (`frozen/surface.rs`, dial → water offset) runs *ahead* of
  the guard; never apply an offset to what the guard returns.
- Config migrations and generated configs must never inject a default alarm
  block — that exact bug fired a real alarm on a real bed (2026-07-20); see
  `.claude/rules/example-configs.md`.
- Owning docs: `docs/CLEANROOM-OS.md` "Bring-up field notes" section;
  `docs/ARCHITECTURE.md` for as-built design.
