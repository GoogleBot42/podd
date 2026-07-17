# podd — open firmware for the Eight Sleep Pod

`podd` is a fully-FOSS replacement for the Eight Sleep Pod's software stack. It
runs entirely on your LAN — no cloud, no OTA phone-home, no account — and is
built to be *hacked on*, not bolted onto the vendor firmware.

It is a **fork of [opensleep](https://github.com/LiamSnow/opensleep)** (the Rust
daemon that drives the Pod's two STM32 microcontrollers directly), extended with
a web UI (forked from [free-sleep](https://github.com/throwaway31265/free-sleep)'s
frontend), a local REST/WebSocket API, a proper thermostat/scheduler, MCU
firmware flashing, and — the part the existing projects get wrong — a **signed,
atomic, reproducible update system**.

> Status: **early scaffolding.** The update tooling (`pod-update` + `podup`) is
> implemented and tested; the opensleep control core is being integrated next.
> See [`docs/REPLACEMENT_PLAN.md`](docs/REPLACEMENT_PLAN.md) for the full design
> and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the target layout.

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

Targets the Eight Sleep **Pod 3** (NXP i.MX8M Mini / Variscite variant today;
the MediaTek no-SD variant and Pod 4/5 differ below the userland — see the plan).
Runs on the stock Yocto base (L1); a full OS-image replacement (L2) is optional
and per-SoC. **No secure boot is enforced on these units**, so custom code runs.

## Workspace

| Crate | Purpose | Status |
|---|---|---|
| `crates/pod-update` | Signed, reproducible update manifests + artifact verification | ✅ implemented + tested |
| `crates/podup` | Host CLI: keygen / pack / release / verify | ✅ implemented |
| `crates/podd` | The control daemon (opensleep fork + API + scheduler + update agent) | 🚧 stub |

Planned additional crates (from the opensleep source map): `pod-proto` (the LSP
UART protocol), `pod-hal` (reset + LED), `api` (axum REST/WS + embedded UI),
`schedule`, `mcu-flash`, `onboarding`. See `docs/ARCHITECTURE.md`.

## Build & test

```sh
cargo test            # unit tests (pod-update crypto/manifest logic)
cargo build           # build podd (stub) + podup

# End-to-end release flow (needs `mksquashfs` on PATH):
podup keygen --out-dir keys
podup release --channel stable --key keys/signing.key --out-dir dist \
    --app-src <built-app-dir> --app-version 0.1.0+abc123 \
    --mcu-frozen firmware-frozen.bbin --mcu-frozen-version 4.2
podup verify --pubkey keys/signing.pub --manifest dist/manifest.json --dir dist
```

The signing key is Ed25519; keep `signing.key` offline. The device bakes in the
public key and refuses any manifest or artifact that doesn't verify.

## License

GPL-3.0-or-later (inherited from opensleep). The vendored web UI is MIT
(free-sleep); attribution preserved. See `LICENSE`.
