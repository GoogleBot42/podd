# `os/` — clean-room OS image (Buildroot external tree)

This is the `BR2_EXTERNAL` tree that builds podd's **clean-room L2 OS image**: a
complete, from-source bootable system (bootloader + kernel + rootfs + podd) for
the Eight Sleep i.MX8M-Mini Variscite "SD" hub, with A/B slots + rollback
driven by the U-Boot env and pod-update-agent.

> **Hardware note:** the SoM is a Variscite **VAR-SOM-MX8M-MINI** (DDR4), *not*
> a DART-MX8M-MINI (LPDDR4) — confirmed from the live device's U-Boot env
> (`board_name=VAR-SOM-MX8M-MINI`, `som_rev13`) and its kernel DTB
> (`imx8mm-var-som-symphony-eight.dtb`). Variscite's `imx8mm_var_dart` U-Boot
> build unifies both SoMs, runtime-selecting DDR timing and control DTB from
> the SOM EEPROM, which is why the boot container must carry **both** DDR
> firmware sets and **both** control DTBs (see below).

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
| **Buildroot** | github.com/buildroot/buildroot | `2026.02.3` | current LTS; ships the imx8mm boot flow, `firmware-imx` 8.27, `libubootenv`, aarch64 toolchain |
| **U-Boot** | github.com/varigit/uboot-imx | `bbb07703` (branch `imx_v2020.04_5.4.70_2.3.0_var01`) | U-Boot v2020.04; board defconfig `imx8mm_var_dart_defconfig` |
| **Linux** | github.com/varigit/linux-imx | `a397cce0` (branch `5.4-2.3.x-imx_var01`) | Linux 5.4.127; config + DTS supplied by this tree (below) |
| **ARM Trusted Firmware** | github.com/varigit/imx-atf | `e5884084` (branch `imx_5.4.70_2.3.0_var01`) | BL31, platform `imx8mm`; the ATF that shipped in this BSP |
| **firmware-imx** | NXP (via Buildroot pkg) | `8.27` | LPDDR4 **and** DDR4 PHY training blobs (NXP redistributable, **not** Eight code); the unsuffixed 8.27 blobs are byte-identical to the ones in the stock boot image |
| **imx-mkimage** | NXP (host, via Buildroot pkg) | Buildroot default | `mkimage_imx8` used to stitch the boot container |

Branch **HEADs** were pinned to their then-current commit SHAs. Re-pin by reading
the branch tip if you want a later BSP dot-fix.

One defconfig pin is easy to miss the reason for:
`BR2_PACKAGE_HOST_LINUX_HEADERS_CUSTOM_5_4=y`
([`configs/podd_imx8mm_varsom_sd_defconfig`](configs/podd_imx8mm_varsom_sd_defconfig#L42)).
Kernel headers normally come from `AS_KERNEL` (the kernel being built), but for
a custom-git kernel tree Buildroot can't infer a version from it and silently
leaves `HEADERS_AT_LEAST` at the 2.6 floor. That's not just cosmetic: glibc
needs headers >= 3.2, so under the floor Buildroot silently falls back from
glibc to uClibc — which then breaks systemd later in the build, with no error
pointing back at the real cause. Pinning the headers series explicitly avoids
the fallback.

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

Our `board/eightsleep/imx8mm-varsom/post-image.sh` assembles the whole i.MX8MM
boot container itself, matching what Variscite's unified DART/VAR-SOM SPL
expects (verified byte-for-byte against the stock Pod 3 SD dump's DDR-firmware
section):

1. **U-Boot** builds `u-boot-nodtb.bin`, `u-boot-spl.bin`, and **both** control
   dtbs (`imx8mm-var-dart-customboard.dtb`, `imx8mm-var-som-symphony.dtb`).
2. **ATF** builds `bl31.bin` (platform `imx8mm`, BL31 base `0x00920000`).
3. **firmware-imx** provides the NXP DDR PHY-training blobs. The SPL is padded
   and gets **both** sets appended — LPDDR4 (`lpddr4_pmu_train_{1d,2d}_{imem,dmem}`)
   at offset 0, DDR4 (`ddr4_{imem,dmem}_{1d,2d}`) at offset **73728**
   (`CONFIG_IMX8M_DDRPHY_FW_OFFSET`); imem slots are 32 KiB, dmem slots 4 KiB.
4. `mkimage_fit_atf.sh` + `mkimage` build a FIT of ATF + U-Boot + both control
   DTBs (SPL picks the config whose *description* matches the EEPROM-detected
   board); `mkimage_imx8 -fit -loader ... 0x7E1000 -second_loader ... 0x40200000
   0x60000` emits **`imx-boot`**.
5. The same script places the container at `0x8400`, bakes the U-Boot env
   (incl. the A/B rollback state machine) at `0x400000`, and runs `genimage`
   to emit `podd-sd.img(.gz)` plus the OTA slot artifact `podd-os.ext4.zst`.

We deliberately do **not** use Buildroot's generic
`board/freescale/common/imx/imx8-bootloader-prepare.sh`: it appends only the
LPDDR4 firmware set and packs only one control DTB — on this DDR4 VAR-SOM the
SPL then trains DDR with garbage read past the image end, and the board is dead
with no console output.

There is **no secure boot** on these units, so the container is unsigned.

## Building

**One command** (from the repo root):

```sh
os/scripts/build.sh
# -> dist/podd-sd.img.gz        the flashable SD image
#    dist/podd-os.ext4.zst      the OTA slot image pod-updater streams
#    dist/podd-rootfs.tar.gz    the same rootfs as a tarball (+ .sha256)
#    (also in build/buildroot/output/images/)
```

The **tarball** is for the consumers that populate a slot by *extracting*
rather than by `dd`ing a filesystem image: `install/podd-slot-install.sh` (eMMC
A/B) and `scripts/build-recovery-sd.sh`. It is not a second build — Buildroot
tars the same `$TARGET_DIR` it builds `rootfs.ext2` from, under fakeroot, and
[`scripts/package-rootfs.sh`](scripts/package-rootfs.sh) renames, content-checks
and checksums it. That script also runs standalone, so an existing Buildroot
tree can re-emit the tarball in seconds without rebuilding:

```sh
os/scripts/package-rootfs.sh --output-dir dist/
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

Verify the write before trusting it — `dd` reporting success doesn't mean the
card actually holds what you sent:

```sh
gunzip -c dist/podd-sd.img.gz > /tmp/podd-sd.img
sudo cmp -n "$(wc -c < /tmp/podd-sd.img)" /tmp/podd-sd.img /dev/sdX && echo OK
```

On a `v*` tag the GitHub release workflow's `os-image` job wraps `build.sh` to
publish these as release assets — see
[../docs/RELEASING.md](../docs/RELEASING.md#the-os-image-lane) for the lane and
its runner budget.

### WiFi provisioning (no baked credentials needed)

If the image boots with **no WiFi profile** (nothing injected at build time,
nothing previously provisioned), `podd-wifi-setup.service` brings up an open
access point **`podd-setup`** on wlan0 and serves a one-field-each SSID/password
form at **http://10.42.0.1/** (busybox httpd + a shell CGI; a wildcard-DNS
`dnsmasq-shared.d` entry makes phone captive-portal detection open the form
automatically). Submitting the form writes a NetworkManager keyfile to
`/run/NetworkManager/system-connections/` (the rootfs slot stays read-only) and
persists a copy in `/data/podd/wifi/`, which the service restores on every boot
— so credentials survive reboots **and** A/B updates. If the join fails (wrong
password), the profile is deleted and the AP comes back for another try.

To re-provision a pod that already has credentials (e.g. a new router), run
`podd-wifi-setup force` over SSH. That subcommand is also the intended hook for
a future physical trigger: the rear factory-reset pinhole button is an input on
the PCAL6416A I²C expander (`0x20` on `/dev/i2c-1`, input port register `0x00`
— the same chip podd's `reset.rs` drives), *not* a gpio-keys input device, so a
button-hold trigger would be a small userspace poller of that register.

The button is currently **inert while podd runs**: `crates/podd-core/src/reset.rs`
only writes the expander's config/output registers (subsystem reset/enable) and
never reads the input port, so nothing in podd polls it. The only things that
still act on it are stock U-Boot's `factory_reset` check and the stock
Capybara daemon — neither of which run once podd owns the system. The exact
bit mapping of the button within input port register `0x00` is undetermined.

### Host mkimage workaround

Buildroot 2026.02.3's own host `u-boot-tools` (mkimage 2025.10) is built with an
empty `CONFIG_MKIMAGE_DTC_PATH`, so its `mkimage` cannot compile the boot FIT and
fails with `-I: command not found`. `build.sh` sidesteps this by passing a
known-good `mkimage` (from `nix build nixpkgs#ubootTools`) to `build-image.sh`
via `PODD_FIT_MKIMAGE`, which shims it over Buildroot's after `host-uboot-tools`
is built. Nothing else in Buildroot is affected.

## Integration (resolved)

The board scripts now close the loop with the pinned build:

1. **Boot-container assembly** — `post-image.sh` builds `imx-boot` itself (see
   ["How `imx-boot` is assembled"](#how-imx-boot-is-assembled)) and hands it to
   `genimage`.
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
    build.sh                                one-command build (nix + FHS sandbox)
    build-image.sh                          fetch Buildroot + nix podd/UI + build
    package-rootfs.sh                       verify/checksum podd-rootfs.tar.gz
  package/podd/                             installs podd binary + UI + service
  board/eightsleep/imx8mm-varsom/
    linux-podd.config                       kernel config seed (sibling-authored)
    imx8mm-podd.dts                         clean-room DTS (sibling-authored)
    genimage.cfg                            A/B + data partition layout
    uboot-env.txt                           boot flow + A/B rollback state machine
    post-build.sh                           /data mount + dtb rename + services
    post-image.sh                           imx-boot + env + genimage -> podd-sd.img
```
