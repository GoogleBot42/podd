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

> Status: **software stack built; hardware bring-up in progress.** The full
> userland (protocol, control core, API, web UI, signed update system, CI, and
> installers) is implemented and tested — 98 tests, reproducible Nix builds,
> static aarch64 binaries. The protocol is **validated against a live Pod 4**
> (both MCUs). Real bed-control writes are gated off (`PODD_DRY_RUN`) pending the
> careful hardware cutover; the Pod-4 sensor packet payloads and the live cutover
> are the remaining work. See [`docs/REPLACEMENT_PLAN.md`](docs/REPLACEMENT_PLAN.md)
> for the full design and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the layout.

## Flashing & updating

Full, beginner-friendly guides live in [`docs/`](docs/):

- **[docs/FLASHING.md](docs/FLASHING.md)** — identify your Pod variant and get root
  (the one-time serial-over-J7 unlock, or the i.MX SD backdoor). Covers hardware to
  buy, exact J7 pinout, and the honest "what's unverified" caveats.
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
curl -fsSL https://raw.githubusercontent.com/eightsleep/podd/main/install/install.sh \
  | sh -s -- --source github:eightsleep/podd
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
