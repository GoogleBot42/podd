---
name: build-sd-image
description: Use when building podd's bootable SD image (the from-source clean-room OS image) or diagnosing why a built image won't boot on the Pod 3-SD / Pod 4 i.MX8M hub. This board has no serial console, JTAG, or USB debug port — post-mortem is entirely via a self-logging diag partition read back on a host. This skill is a thin pointer to already-automated scripts plus the gotchas those scripts don't tell you; read os/README.md and docs/CLEANROOM-OS.md for the architecture first.
---

# Building & debugging the podd SD image

The mechanical build steps are already automated — this skill exists for the
gotchas, not to re-explain the scripts. Read `os/README.md` (L2 architecture,
pinned versions, layout) and `docs/CLEANROOM-OS.md` (why it exists, the
clean-room boundary, bring-up field notes) before or alongside this.

## The build

`os/scripts/build.sh` → `dist/podd-sd.img.gz` (also left under
`build/buildroot/output/images/`). Builds bootloader + kernel + rootfs + podd
entirely from pinned upstream source via Buildroot; contains zero Eight Sleep
code; proven on hardware (`docs/CLEANROOM-OS.md`). `build.sh` wraps the
lower-level `os/scripts/build-image.sh` (CI calls the latter directly;
`--help` lists its flags: `--buildroot DIR`, `--no-nix` +
`--podd-bin`/`--ui-dir`, `--jobs N`, `--output-dir`).

(The legacy L1 stock-clone image and its `scripts/build-podd-sd.sh` /
`slim-podd-sd.sh` / `patch-podd-sd-diag.sh` family were removed from the repo
2026-08-31 — recover from git history if ever needed.)

Write to a **spare** SD card; the stock card stays untouched as an instant
revert (see `docs/RECOVERY.md`, "SD-swap boot").

## Gotchas the scripts don't tell you

### 1. The from-source bootloader now boots — but the splice trick is still a useful bisector

Historically the from-source `imx-boot` chain didn't boot at all (silent —
no console to show why). That's fixed: `docs/CLEANROOM-OS.md` confirms L2's
from-source SPL/ATF/U-Boot boots on real hardware as of 2026-07-20. But if
you're modifying the bootloader assembly (`os/board/eightsleep/imx8mm-varsom/
post-image.sh`) and a rebuilt image goes dead again, the technique that
originally isolated bootloader-vs-rootfs failures is still the fastest way to
bisect: splice just the bootloader region from a known-good image into the
new one and see if that alone revives it.

```sh
dd if=<known-good>.img of=<target>.img bs=512 skip=66 seek=66 count=8126 conv=notrunc
```

Sector 66 = byte offset `0x8400` (where `imx-boot` starts); 8126 sectors
reaches exactly byte `0x400000` (where the U-Boot env starts) — so this
copies precisely the `imx-boot` region and touches neither the env nor any
rootfs partition. (These numbers are load-bearing, not approximate.) If the spliced
image boots and the un-spliced one didn't, the bug is in your bootloader
assembly, not the kernel/rootfs; if it still doesn't boot, look upstream of
the bootloader region (DDR training firmware, DTB selection).

### 1b. Verify the image actually contains your new podd/UI — stamps lie

The `podd` Buildroot package is install-only (the binary/UI are built outside
Buildroot), and Buildroot's stamps don't content-track those external
artifacts. `build-image.sh` now deletes `.stamp_target_installed` before every
make (2026-08-15) so this should not recur — but a rebuilt image that
mysteriously lacks your change means the stamp mechanism ate the reinstall.
Verify shipped contents against `output/images/rootfs.tar` instead of trusting
the build log, e.g.:

```sh
tar -xOf build/buildroot/output/images/rootfs.tar ./usr/bin/podd \
  | grep -a -c "<some string only the new binary contains>"
tar -xOf build/buildroot/output/images/rootfs.tar ./usr/share/podd/ui/index.html \
  | grep -o 'index-[^"]*\.js'   # must reference the bundle hash you just built
```

### 1c. You can run the image's aarch64 userland on the build host

The build host has qemu-user binfmt, so image binaries run directly if you
give them the image's own loader — no hardware needed. Used 2026-08-15 to
reproduce (and then verify the fix for) the provisioning portal's 404 by
running the image's actual busybox httpd against the real www dir:

```sh
mkdir t && cd t
tar -xf .../images/rootfs.tar ./usr/bin/busybox ./usr/lib/ld-linux-aarch64.so.1 \
    ./usr/lib/libc.so.6 ./usr/lib/libm.so.6 ./usr/lib/libresolv.so.2
./usr/lib/ld-linux-aarch64.so.1 --library-path ./usr/lib ./usr/bin/busybox httpd ...
```

Reach for this before flashing when a userland behavior (not kernel/driver)
is in question — a flash-boot-pull-card cycle takes 15+ minutes; this takes
seconds. Corollary from the same day: features whose code paths hardware
never exercised (AP mode vs STA, portal vs baked creds) tend to hide stacked
config gaps — nmcli missing, wpa_supplicant CONFIG_AP off, busybox httpd
index bug — so test the actual feature path, not just file presence.

### 2. Host `mkimage` is broken in this Buildroot release

Buildroot 2026.02.3's own host `u-boot-tools` (mkimage 2025.10) can't compile
the boot FIT (`-I: command not found`) because of an empty
`CONFIG_MKIMAGE_DTC_PATH`. `os/scripts/build.sh` already works around this by
shimming in a known-good `mkimage` from `nix build nixpkgs#ubootTools` via
`PODD_FIT_MKIMAGE`/`PODD_FIT_MKIMAGE_LIBS`. If you call `build-image.sh`
directly (bypassing `build.sh`) and hit this error, you skipped the shim —
see "Host mkimage workaround" in `os/README.md`.

### 3. Writing to SD

- **Verify the target is actually the SD card, not a disk you care about**
  before any `dd`. The commands below use `/dev/sdX` as a placeholder — that
  placeholder has bitten people before.
- **Counterfeit/fake-capacity SD cards are a real failure mode for this
  project** — a lying card was the actual root cause of one "device never
  comes up" incident. Symptom: early `No space left on device` during `dd`
  despite the card reporting full capacity. See the "verify your SD card"
  callout at the top of the safety nets in `docs/RECOVERY.md` (`lsblk -b`
  for claimed size; `f3probe --destructive` on spare cards to confirm).
- **Always verify after writing:** `cmp -n <byte-count> image.img /dev/sdX`
  — this is a root-`CLAUDE.md` safety tripwire (raw media writes must be
  verified).

### 4. If it won't boot: no serial console, no JTAG, no USB — read the diag partition

This board has none of the usual debug ports (`docs/CLEANROOM-OS.md` →
"Debug channels"). Post-mortem is entirely: boot the image, let it run a few
minutes, power off, pull the card, mount it **read-only** on a host, and read
the boot log it wrote to its own persistent storage.

Diagnostics are baked into every build automatically — no separate patch
step. The logger
(`os/board/eightsleep/imx8mm-varsom/rootfs-overlay/usr/bin/podd-bootlog`)
and its units (`podd-bootlog-early.service` at `sysinit.target`,
`podd-bootlog.timer` firing `podd-bootlog-late.service` `OnBootSec=60s`)
are enabled by `post-build.sh` into every rootfs. It writes to
`/data/bootlog` — **that's partition p3** (`genimage.cfg`: the `data`
partition, ext4, `label = "podd_data"`), **not p1**. Boot ~3 min (past the
60 s late snapshot), power off, then on a host (device node depends on your
reader: `/dev/sdX3` for a USB adapter, `/dev/mmcblk0p3` for a built-in slot):

```sh
sudo mount -o ro /dev/sdX3 /mnt   # p3, label podd_data — NOT p1
ls -la /mnt/bootlog/              # status-early.txt, status-late.txt,
                                   # dmesg-early.txt, dmesg-late.txt,
                                   # journal-early.txt, journal-late.txt,
                                   # wifi-early.txt, wifi-late.txt
sudo umount /mnt
```

If `/data` failed to mount on-device (e.g. p3 didn't come up at all), the
logger falls back to `/root/bootlog` on the rootfs itself — check p1 for
that if p3 comes up empty or doesn't exist. There is no "mid" stage file —
only `early`/`late`.

### 5. TZDIR / jiff zoneinfo crash-loop

Symptom: podd crash-loops parsing any `timezone:` field in `config.ron`,
even though the zoneinfo files are clearly present on the rootfs. Cause:
Buildroot's tz-info package installs top-level zone names
(`/usr/share/zoneinfo/America`, etc.) as **symlinks** into `posix/`, and
podd's tz library (`jiff`) does not follow symlinked zone directories — nor
does it fall back to its own bundled tzdb while a system zoneinfo dir is
present. Net effect: every IANA name fails to resolve despite the files
being right there.

`os/board/eightsleep/imx8mm-varsom/post-build.sh` already works around this
at build time by replacing the top-level symlinks with real hardlinked
copies (`cp -al`), and it **fails the build loudly**
(`FATAL zoneinfo de-symlink failed`) if `America/New_York` doesn't come out
resolvable afterward — so a build produced by the current `os/scripts/
build.sh` should not hit this. If you see this crash-loop live on a device
anyway (e.g. an older image built before this fix existed), the field
workaround was a `TZDIR=` systemd drop-in on `podd.service` — see
`CLAUDE.local.md` (untracked) for the live device's exact drop-in; don't
hardcode a path here since it's a workaround for an already-fixed build
issue, not the current expected state.
