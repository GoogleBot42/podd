# podd — open firmware for the Eight Sleep Pod

`podd` is a fully-FOSS replacement for the Eight Sleep Pod's software stack. It
runs entirely on your LAN — no cloud, no OTA phone-home, no account — and is
built to be *hacked on*, not bolted onto the vendor firmware.

It is a **fork of [opensleep](https://github.com/LiamSnow/opensleep)** (the Rust
daemon that drives the Pod's two STM32 microcontrollers directly), extended with
a web UI (forked from [free-sleep](https://github.com/throwaway31265/free-sleep)'s
frontend), a local REST API (with server-sent-event log streaming), a
thermostat/scheduler, MCU firmware flashing, and — the part the existing
projects get wrong — a **signed, atomic, reproducible update system**.

> Status: **running in production on real hardware (2026-07-20).** podd boots
> from the clean-room OS image — from-source bootloader/kernel/rootfs, **zero
> Eight Sleep binaries** ([docs/CLEANROOM-OS.md](docs/CLEANROOM-OS.md)) — and
> **drives a live Pod nightly**: both bed sides tested on/off and holding their
> setpoints (live MCU writes, `PODD_DRY_RUN=false`), scheduler, API, and web UI
> all exercised against hardware. The full userland is unit-tested throughout
> (100+ tests) with reproducible Nix builds and static aarch64 binaries.
> OS A/B OTA is wired end-to-end (updater writes + verifies the inactive SD
> slot, U-Boot counts boot attempts and auto-reverts, podd marks-good —
> hardware verification of the full cycle pending). Biometrics (heart rate,
> HRV, breathing rate, sleep sessions, and movement from the piezo +
> capacitance streams) are computed and recorded on-device. Remaining work:
> the OS-OTA hardware pass, validating the biometrics numbers against a
> reference (issue #142), and an
> open reliability item on the Pod-4 sensor MCU (auto-recovers; see
> `docs/research/pod4-sensor-protocol.md` §5). See
> [`docs/REPLACEMENT_PLAN.md`](docs/REPLACEMENT_PLAN.md) for the full design
> and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the layout.

## Flashing & updating

Full, beginner-friendly guides live in [`docs/`](docs/):

- **[docs/FLASHING.md](docs/FLASHING.md)** — identify your hub and get in. The
  unlock path is **board-specific**: serial-over-J7 on the MediaTek hub; the SD
  paths on the i.MX "SD" hub (whose JTAG-footprint header is real JTAG, *not* a
  UART). Covers hardware to buy, pinouts, and the honest "what's unverified"
  caveats.
- **[docs/CLEANROOM-OS.md](docs/CLEANROOM-OS.md)** — the primary, validated
  i.MX install method: `os/scripts/build.sh` builds a complete from-source OS
  image (no Eight Sleep binaries); dd it to a microSD, swap it in, eMMC
  untouched; swap the stock card back to revert.
- **[docs/SD-BOOT.md](docs/SD-BOOT.md)** — the boot-flow analysis behind the
  SD-swap model, plus the legacy L1 stock-clone image (superseded).
- **[docs/INSTALL.md](docs/INSTALL.md)** — install podd once you have root (the
  one-command userland install; the advanced A/B slot install).
- **[docs/UPDATING.md](docs/UPDATING.md)** — the on-device OTA agent: sources,
  channels, auto/manual, rollback, and the owner-controlled trust policy.
- **[docs/RECOVERY.md](docs/RECOVERY.md)** — unbrick / go back to stock, cheapest
  net first.
- **[docs/RELEASING.md](docs/RELEASING.md)** — maintainer notes: cutting a release
  and (optional) CI signing.

**Already rooted?** (You run free-sleep / opensleep and have SSH.) Skip straight to
the one-command install — from a root shell on the Pod:

```sh
curl -fsSL https://git.neet.dev/zuckerberg/podd/raw/branch/main/install/install.sh \
  | sh -s -- --source gitea:https://git.neet.dev/zuckerberg/podd
```

Then open `http://<pod-ip>:3000`. podd installs in safe **dry-run** mode (it logs
hardware writes instead of sending them) until you deliberately arm it — see
[docs/INSTALL.md](docs/INSTALL.md).

## Why not just use free-sleep / opensleep?

- **free-sleep** bolts a Node/Express server onto Eight's still-running
  `frankenfirmware` and updates by `git pull`-ing prebuilt JS onto the device.
  Good UI, wrong foundation.
- **opensleep** correctly deletes Eight's stack and talks to the MCUs directly,
  but has no web UI, no scheduler beyond a single daily curve, and no update
  story.

`podd` keeps opensleep's clean hardware core, adds free-sleep's UI on top of a
native Rust API, and treats **updates as a first-class, verifiable concern**.

## Hardware scope

The reference hardware is the NXP i.MX8M Mini / Variscite "SD" **hub** (what the
Eight Sleep app labels varies — it reports the mattress *cover*, not the hub; see
[docs/FLASHING.md](docs/FLASHING.md#step-1--identify-your-pod) for identifying
yours). The i.MX "no-SD" (Pod 4) hub and the MediaTek no-SD hub differ below the
userland — see [`docs/REPLACEMENT_PLAN.md`](docs/REPLACEMENT_PLAN.md). The
primary, validated install is the from-source clean-room OS image (L2) on a
swappable microSD ([docs/CLEANROOM-OS.md](docs/CLEANROOM-OS.md)); podd also
still runs as a userland-only install on the stock Yocto base (L1), which is
what the one-command install above does on an already-rooted unit and what the
legacy stock-clone image ([docs/SD-BOOT.md](docs/SD-BOOT.md)) shipped.
**No secure boot is enforced on these units**, so custom code runs.

## Workspace

| Crate | Purpose | Status |
|---|---|---|
| `crates/pod-update` | Signed, reproducible update core (manifests + SHA-256/Ed25519 verification), shared by host and device | ✅ implemented + tested |
| `crates/podup` | Host release CLI: `keygen` / `pack` / `release` / `verify` | ✅ implemented |
| `crates/pod-proto` | LSP UART protocol (framing/CRC, Frozen + Sensor packet/command tables, thermostat `profile.rs`), extracted from opensleep | ✅ implemented + tested; validated vs live Pod 4 |
| `crates/podd-core` | opensleep control core: Frozen/Sensor subsystems, LED, reset, config, MQTT, state bus | ✅ implemented |
| `crates/api` | free-sleep-compatible REST + SSE HTTP API and embedded-SPA server (biometrics endpoints deferred) | ✅ implemented |
| `crates/pod-updater` | On-device OTA agent: Tier-2 app swaps and Tier-1 OS A/B slot updates (write + readback-verify + U-Boot-armed trial with auto-rollback) are live, both behind dry-run gates; Tier-3 MCU apply is still gated | ✅ implemented |
| `crates/pod-probe` | Read-only serial probe for validating `pod-proto` against live MCUs | ✅ implemented |
| `crates/podd` | The control daemon: wires `podd-core` + `api` + `pod-updater` together; MCU writes gated behind `PODD_DRY_RUN` | ✅ implemented (live hardware cutover pending) |

Note on the earlier opensleep source map: `pod-hal` (reset + LED) is folded
into `podd-core`; MCU `.bbin` flashing lives in `pod-updater` (Tier 3,
dry-run-gated); schedule persistence and endpoints live in `api`, with the
thermostat curve in `pod-proto`'s `profile.rs`. Still genuinely **planned**:
WiFi/onboarding bring-up, a full autonomous weekday scheduler loop, and the L2
OS-image (`podd-rootfs.tar.gz`) release. See `docs/ARCHITECTURE.md`.

## Build & test

```sh
cargo test            # workspace unit tests (protocol, crypto/manifest, api, updater)
cargo build           # build the workspace (podd, podup, …)

# End-to-end release flow (needs `mksquashfs` on PATH):

# Unsigned — push your own build to your own device (digests still checked):
podup release --channel dev --out-dir dist \
    --app-src <built-app-dir> --app-version 0.1.0+abc123
podup verify --manifest dist/manifest.json --dir dist

# Signed — with your OWN self-generated key (optional):
podup keygen --out-dir keys
podup release --channel stable --key keys/signing.key --out-dir dist \
    --app-src <built-app-dir> --app-version 0.1.0+abc123 \
    --mcu-frozen firmware-frozen.bbin --mcu-frozen-version 4.2
podup verify --pubkey keys/signing.pub --manifest dist/manifest.json --dir dist
```

**Signing is optional and owner-controlled.** Artifact integrity (SHA-256) is
always enforced; a signature adds *authenticity*. The device owner chooses the
trust policy (`AllowUnsigned` / `RequireSigned(keys)`), so you can hack on your
own device unsigned, or trust your own self-generated key(s) — no central
authority. Keep any `signing.key` offline.

## License

GPL-3.0-or-later (inherited from opensleep). The vendored web UI is MIT
(free-sleep); attribution preserved. See `LICENSE`.
