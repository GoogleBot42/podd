# `os/` — clean-room OS image (Buildroot external tree)

This is the `BR2_EXTERNAL` tree that builds podd's **clean-room L2 OS image**: a
complete, from-source bootable system (bootloader + kernel + rootfs + podd) for
the Eight Sleep i.MX8M-Mini Variscite "SD" hub (a Variscite **DART-MX8M-MINI**
SoM, `compatible = "variscite,dart-mx8mm"`), updated A/B via RAUC. It replaces
the L1 "bolt podd onto Eight's Yocto rootfs" approach (`scripts/build-podd-sd.sh`).

See **[../docs/CLEANROOM-OS.md](../docs/CLEANROOM-OS.md)** for the architecture,
the clean-room boundary, and the slot/partition layout.

> **Status: buildable configuration, not yet booted.** The upstream source
> revisions are pinned (below) against the exact Variscite BSP the live stock
> device runs, so this rebuilds a *supported* board. Two integration one-liners
> live in board scripts that are outside this config's ownership — see
> [Integration handoffs](#integration-handoffs). Bring-up is gated by the **no
> reachable serial console** on this board — validate via WiFi/SSH + the
> self-logging diag partition.

## Pinned versions

The build is reproducible from these pins. Upstream source pins live in
[`configs/podd_imx8mm_varsom_sd_defconfig`](configs/podd_imx8mm_varsom_sd_defconfig);
the Buildroot pin lives in [`scripts/build-image.sh`](scripts/build-image.sh).
They correspond to the **Variscite Yocto Hardknott v1.4** BSP (Linux 5.4.127),
which is what the stock device was built from.

| Component | Repo | Rev / tag | Notes |
|---|---|---|---|
| **Buildroot** | github.com/buildroot/buildroot | `2026.02.3` | current LTS; ships the imx8mm boot flow, `firmware-imx` 8.27, `rauc`, aarch64 toolchain |
| **U-Boot** | github.com/varigit/uboot-imx | `bbb07703` (branch `imx_v2020.04_5.4.70_2.3.0_var01`) | U-Boot v2020.04; board defconfig `imx8mm_var_dart_defconfig` |
| **Linux** | github.com/varigit/linux-imx | `a397cce0` (branch `5.4-2.3.x-imx_var01`) | Linux 5.4.127; config + DTS supplied by this tree (below) |
| **ARM Trusted Firmware** | github.com/varigit/imx-atf | `e5884084` (branch `imx_5.4.70_2.3.0_var01`) | BL31, platform `imx8mm`; the ATF that shipped in this BSP |
| **firmware-imx** | NXP (via Buildroot pkg) | `8.27` | LPDDR4 PHY training + HDMI blobs (NXP redistributable, **not** Eight code) |
| **imx-mkimage** | NXP (host, via Buildroot pkg) | Buildroot default | `mkimage_imx8` used to stitch the boot container |

Branch **HEADs** were pinned to their then-current commit SHAs. Re-pin by reading
the branch tip if you want a later BSP dot-fix.

### Board-specific config supplied by this tree

Authored under `board/eightsleep/imx8mm-varsom/` (kernel `.config` and DTS are
produced by sibling work; the defconfig already references them by path):

- **`linux-podd.config`** — kernel config seed (trimmed from the stock device
  `.config`). Wired via `BR2_LINUX_KERNEL_CUSTOM_CONFIG_FILE`.
- **`imx8mm-podd.dts`** — clean-room device tree: DART base + New-Rat carrier
  deltas (UART1 sensor MCU `ttymxc0`, UART3 frozen MCU `ttymxc2`; I²C PMIC/RTC/
  LED/GPIO-expander; Ethernet off — no PHY populated). Wired via
  `BR2_LINUX_KERNEL_CUSTOM_DTS_PATH`.

## How `imx-boot` is assembled

The i.MX8MM boot container is built exactly the way Buildroot's reference
`freescale_imx8mmevk` board does it (proven flow), with Variscite source swapped
in:

1. **U-Boot** builds `u-boot-nodtb.bin`, `u-boot-spl.bin`, and the DART control
   dtb (`imx8mm-var-dart-customboard.dtb`).
2. **ATF** builds `bl31.bin` (platform `imx8mm`, BL31 base `0x00920000`).
3. **firmware-imx** installs the NXP LPDDR4 PHY-training blobs and links them as
   `ddr_fw.bin`; **host imx-mkimage** installs `mkimage_imx8` / `mkimage_fit_atf.sh`.
4. Buildroot's stock `board/freescale/common/imx/imx8-bootloader-prepare.sh`
   (first post-image script) stitches SPL+DDR-fw + a FIT of ATF+U-Boot into
   **`output/images/imx8-boot-sd.bin`** — the boot container.
5. Our `board/eightsleep/imx8mm-varsom/post-image.sh` places that container at
   `0x8400`, bakes the RAUC U-Boot env at `0x400000`, and runs `genimage` to emit
   `podd-sd.img(.gz)`.

There is **no secure boot** on these units, so the container is unsigned.

## Building

**One command** (from the repo root):

```sh
os/scripts/build.sh
# -> dist/podd-sd.img.gz   (also build/buildroot/output/images/)
```

`build.sh` orchestrates the whole thing: it Nix-builds the podd binary, the web
UI, and the Buildroot **FHS sandbox**, then runs Buildroot *inside* that sandbox
(Buildroot hardcodes FHS paths a plain `nix shell` can't provide — see the
`buildrootEnv` package in `flake.nix`). The heavy compile (toolchain, ATF,
U-Boot, kernel, rootfs) is cached under `build/buildroot/` after the first run.

The one-command script wraps the lower-level `build-image.sh`, which does the
Buildroot side only and is what CI calls. `build-image.sh --help` lists its
flags (`--buildroot DIR` to reuse a checkout, `--no-nix --podd-bin/--ui-dir` for
prebuilt artifacts, `--jobs N`).

Write the result to a **spare** SD (the stock card stays your instant revert):

```sh
gunzip -c dist/podd-sd.img.gz \
  | sudo dd of=/dev/sdX bs=4M conv=fsync status=progress; sync
```

CI wraps `build.sh` to publish `podd-sd-<version>.img.gz` + the RAUC bundle on
tag releases (replacing the `recovery-sd` stub job).

### Host mkimage workaround

Buildroot 2026.02.3's own host `u-boot-tools` (mkimage 2025.10) is built with an
empty `CONFIG_MKIMAGE_DTC_PATH`, so its `mkimage` cannot compile the boot FIT and
fails with `-I: command not found`. `build.sh` sidesteps this by passing a
known-good `mkimage` (from `nix build nixpkgs#ubootTools`) to `build-image.sh`
via `PODD_FIT_MKIMAGE`, which shims it over Buildroot's after `host-uboot-tools`
is built. Nothing else in Buildroot is affected.

## Integration (resolved)

The board scripts now close the loop with the pinned build:

1. **Boot-container naming** — `post-image.sh` copies the prepare script's
   `imx8-boot-sd.bin` → `imx-boot` (the name `genimage.cfg` references) before
   laying out the image.
2. **Kernel + DTB into the slot** — `post-build.sh` stages `Image.gz` and the
   DTB (renamed `imx8mm-podd.dtb` → `/boot/podd.dtb`) into each rootfs slot's
   `/boot`, which is where `uboot-env.txt` loads them from (U-Boot reads the
   ext4 slot directly).

## Layout

```
os/
  external.desc / external.mk / Config.in   BR2_EXTERNAL plumbing
  configs/
    podd_imx8mm_varsom_sd_defconfig         board defconfig (pinned)
  scripts/
    build-image.sh                          fetch Buildroot + nix podd/UI + build
  package/podd/                             installs podd binary + UI + service
  board/eightsleep/imx8mm-varsom/
    linux-podd.config                       kernel config seed (sibling-authored)
    imx8mm-podd.dts                         clean-room DTS (sibling-authored)
    genimage.cfg                            A/B + data partition layout
    rauc-system.conf                        RAUC slots (rootfs_a / rootfs_b)
    uboot-env.txt                           BOOT_ORDER / bootcount A/B selection
    post-build.sh                           /data mount + RAUC config + dtb rename
    post-image.sh                           imx-boot + env + genimage -> podd-sd.img
```
