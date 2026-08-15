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
- LSP framing has no byte-stuffing: a `0x7E` byte anywhere in a frame gets it
  silently dropped, no echo, no error. Setpoints must go through
  `FrozenTarget::delimiter_safe` (`crates/pod-proto/src/frozen/command.rs`);
  compare the MCU's echo against the nudged value, not the requested one.
- All MCU control writes must respect the `PODD_DRY_RUN` gate
  (`crates/podd/src/main.rs`); it defaults to dry-run (log, don't send).
- Alarms must never arm before NTP sync; there is no RTC battery.
- Config migrations and generated configs must never inject a default alarm
  block — that exact bug fired a real alarm on a real bed (2026-07-20); see
  `.claude/rules/example-configs.md`.
- Owning docs: `docs/CLEANROOM-OS.md` "Bring-up field notes" section;
  `docs/ARCHITECTURE.md` for as-built design.
