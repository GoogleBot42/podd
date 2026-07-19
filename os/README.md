# `os/` — clean-room OS image (Buildroot external tree)

This is the `BR2_EXTERNAL` tree that builds podd's **clean-room L2 OS image**: a
complete, from-source bootable system (bootloader + kernel + rootfs + podd) for
the Eight Sleep i.MX8M-Mini Variscite "SD" hub, updated A/B via RAUC. It replaces
the L1 "bolt podd onto Eight's Yocto rootfs" approach (`scripts/build-podd-sd.sh`).

See **[../docs/CLEANROOM-OS.md](../docs/CLEANROOM-OS.md)** for the architecture,
the clean-room boundary, and the slot/partition layout.

> **Status: scaffold — not yet built or booted.** The tree structure, RAUC
> config, partition layout, and U-Boot A/B boot logic are here and reviewable.
> Values that need a real build + hardware to pin (U-Boot/kernel source pins,
> the device tree, the imx-mkimage target, the exact kernel image format) are
> marked `TODO(bring-up)`. Bring-up is gated by the **no reachable serial
> console** on this board — validate via the self-logging diag + JTAG.

## Layout

```
os/
  external.desc / external.mk / Config.in   BR2_EXTERNAL plumbing
  configs/
    podd_imx8mm_varsom_sd_defconfig         board defconfig (starting point)
  package/podd/                             installs podd binary + UI + service
  board/eightsleep/imx8mm-varsom/
    genimage.cfg                            A/B + data partition layout
    rauc-system.conf                        RAUC slots (rootfs_a / rootfs_b)
    uboot-env.txt                           BOOT_ORDER / bootcount A/B selection
    post-build.sh                           /data mount + RAUC config into rootfs
    post-image.sh                           imx-boot + env + genimage -> podd-sd.img
```

## Building (once bring-up TODOs are resolved)

podd itself is cross-compiled outside Buildroot (reproducible Nix cross-build),
then staged by the `podd` package:

```sh
nix build .#podd-aarch64        # -> result-podd/bin/podd
nix build .#ui                  # -> result-ui

git clone https://git.buildroot.net/buildroot   # pinned rev: TODO(bring-up)
make -C buildroot BR2_EXTERNAL=$PWD/os podd_imx8mm_varsom_sd_defconfig
make -C buildroot \
  PODD_BIN=$PWD/result-podd/bin/podd \
  PODD_UI_DIR=$PWD/result-ui
# -> buildroot/output/images/podd-sd.img.gz
```

CI will wrap this to publish `podd-sd-<version>.img.gz` + the RAUC bundle on tag
releases (replacing the `recovery-sd` stub job).
