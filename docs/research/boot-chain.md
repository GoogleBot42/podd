# Eight Sleep Pod 3 — Boot Chain & Partition Layout Analysis

Source image: `eightsleep-pod3-sdcard-original-firmware.img.xz`
Decompressed: `.../work/sdcard/pod3.img` — **15,931,539,456 bytes (~14.84 GiB)**, sparse (~511 MB actual).
SoC/board: NXP **i.MX8M Mini** on **Variscite VAR-SOM-MX8M-MINI (DART-MX8M-MINI SoM)**, custom Eight Sleep carrier ("EightSleep New-Rat 0.8").

---

## 1. Partition Table (MBR / DOS, disk id 0x51a45943)

Sector size 512 B. First 8 MiB (sectors 0–16383) reserved for the bootloader (no partition).

| Part | Start sector | Byte offset  | Size    | FS   | Label  | Role |
|------|--------------|--------------|---------|------|--------|------|
| p1   | 16384        | 0x0080_0000  | 6.1 GiB | ext4 | `A`    | **rootfs A** |
| p2   | 12804096     | 0x1_869F_0000| 6.1 GiB | ext4 | `B`    | **rootfs B** |
| p3   | 25595904     | 0x3_0D80_0000| 444 MiB | ext4 | `cage` | **persistent /cage** |

All three confirmed ext4 (superblock magic 0x53EF at part+0x438).
`/etc/fstab`: `/dev/mmcblk2p3 → /cage` (nofail,noatime,discard). The eMMC enumerates as **mmcblk2** in Linux; `/deviceinfo` is a 2 MB tmpfs. Root `/` is the active A or B partition (mmcblk2p1 / mmcblk2p2).

No dedicated boot/kernel partition: **kernel + DTB live in `/boot` of each rootfs partition** and are loaded by U-Boot from the same partition it will boot as root (A/B kept in lockstep).

---

## 2. Bootloader (imx-boot / flash.bin)

Location in image: MBR at 0x0; **imx-boot container written at sector 0x40 (0x8000)**; SPL body begins ~0xC000.

- **U-Boot SPL 2020.04-imx_v2020.04_5.4.70_2.3.0_var01+g0cdd02af90 (Feb 18 2022)** — Variscite BSP.
- **U-Boot proper 2020.04-imx_v2020.04_5.4.70_2.3.0_var01+g0cdd02af90 (Feb 18 2022)**.

### Secure boot (HABv4) — NOT ENFORCED  → **GO for custom firmware**
- SPL **HABv4 IVT** found at file **0x8400**: header `D1 00 20 41`, entry `0x007E0010`, self `0x007E0FC0`, boot_data `0x007E0FE0` (image base `0x007E0BC0`, length `0x0003DA60`), **CSF pointer `0x0081C5C0` (non-zero)**.
- The CSF pointer is set (image built with the signing layout) **but the CSF/signature region it points to — file offset 0x43A00 — is entirely zero.** i.e. **no CSF, no SRK table, no signature is attached.** The shipped SPL is **unsigned**.
- Consequence: the i.MX8MM boot ROM on these units is running an **unsigned SPL**, which is only possible on an **HAB-open (non-closed) SoC**. Therefore secure boot is not enforced and a **custom / unsigned bootloader + kernel will boot**.
- Caveat: SoC OTP fuse state (SRK_HASH / SEC_CONFIG) cannot be read from an SD image. But since the factory image itself carries no signature, production devices provably execute unsigned code — the bootloader is **not locked**.

### U-Boot environment (default env compiled into U-Boot; A/B logic)
Two default-env variants are baked in (one per rootfs slot). Key vars:

```
mmcdev=1 / 2          mmcpart=1 / 2         mmcblk=1 / 2      next_mmcpart=2
bootcount=0           bootlimit=3           ustate=0         upgrade_available=0
bootdir=/boot         image=Image.gz        img_addr=0x42000000  loadaddr=0x40480000
fdt_addr=0x43000000   fdt_file=imx8mm-var-som-symphony-eight.dtb   boot_fdt=try
script=boot.scr       console=undefined     baudrate=115200  fdt_high=0xffffffffffffffff
setconsole = if console=undefined -> setenv console ttymxc3,115200
mmcargs = ... root=/dev/mmcblk${mmcblk}p${mmcpart} rootwait rw ${cma_size}
loadfdt  = load mmc ${mmcdev}:${mmcpart} ${fdt_addr} ${bootdir}/${fdt_file}
loadimage= load mmc ${mmcdev}:${mmcpart} ${img_addr} ${bootdir}/${image}; unzip -> loadaddr
mmcboot  = run mmcargs; run optargs; loadfdt; booti ${loadaddr} - ${fdt_addr}
```

**A/B selection & rollback (bootcount/bootlimit):**
```
bootcmd    = ... set cma=640M@1376M; mmc dev ${mmcdev}; run factory_reset;
             mmc rescan; loadbootscript||loadimage -> mmcboot
altbootcmd = echo Rollback to previous mmcpart=${mmcpart};
             if mmcpart==1 then mmcpart=2 else mmcpart=1;
             setenv bootcount 0; saveenv; run bootcmd
```
- `mmcpart` (1 or 2) selects **both** the rootfs partition **and** the `/boot` dir the kernel+DTB are loaded from.
- U-Boot increments `bootcount` each boot; if it exceeds `bootlimit=3` the ROM/U-Boot runs `altbootcmd`, which **flips mmcpart to the other slot**, clears bootcount, saves env, and reboots → automatic A/B failover. Userspace (SWUpdate/`defib`) resets `bootcount`/`ustate` to confirm a good boot.
- `factory_reset`: probes an I2C button on U-Boot bus 1 (GPIO expander @0x20, LED controller @0x53); if the button is held it forces `mmcpart=1, mmcdev=1` (revert to slot A) and saves env.
- `ustate` = SWUpdate update-state variable (0 = OK). `swupdate.sh` blocks the update until `get_ustate == STATE_OK` ("waiting for defib to mark the update ok").

Console/debug UART default = **ttymxc3, 115200** (matches DTB `stdout-path`).

---

## 3. Kernel + DTB

- Kernel: **`/boot/Image.gz`** (gzipped arm64 Image), version `5.4.127+g168dacbd9f51`.
- DTB: **`/boot/imx8mm-var-som-symphony-eight.dtb`** (v17, 41,676 B).
  - `compatible = "variscite,dart-mx8mm", "fsl,imx8mm"`
  - `model = "Variscite VAR-SOM-MX8M-MINI on EightSleep New-Rat 0.8"`
- Cortex-M4 remote-proc firmware present in `/boot`: `cm_rpmsg_lite_*`, `cm_hello_world.bin*` (RPMsg co-processor).

### DTB peripheral map (decompiled with a custom FDT parser; addresses are fixed i.MX8MM)

**UARTs** (Linux `ttymxcN` follows `serialN` alias order):

| alias  | node             | Linux dev | status  | notes |
|--------|------------------|-----------|---------|-------|
| serial0 | serial@30860000 | **ttymxc0** | okay | UART1, DMA rx/tx, **no flow control** → sensor/pump data line |
| serial1 | serial@30890000 | ttymxc1 | okay | UART2, **uart-has-rtscts** (RTS/CTS) → Bluetooth/module |
| serial2 | serial@30880000 | **ttymxc2** | okay | UART3, DMA rx/tx, **no flow control** → sensor/pump data line |
| serial3 | serial@30a60000 | ttymxc3 | okay | UART4, **console** (`chosen/stdout-path`) |

→ The two **sensor/pump UARTs are ttymxc0 (0x30860000) and ttymxc2 (0x30880000)** — both plain 2-wire UARTs with DMA and no hardware flow control. The DTB does not name which is pump vs. sensor; that binding is in userspace (`defib`). ttymxc1 has RTS/CTS (BT/radio); ttymxc3 is the serial console.

**I2C buses** (alias `i2cN` → Linux bus N):

| alias | node          | status   | devices |
|-------|---------------|----------|---------|
| i2c0  | i2c@30a20000  | okay     | `ti,sn65dsi83` DSI→LVDS bridge @0x2c; **`rohm,bd71847` PMIC @0x4b** |
| i2c1  | i2c@30a30000  | okay     | `microcrystal,rv3028` RTC @0x68 |
| i2c2  | i2c@30a40000  | okay     | `wlf,wm8904` audio codec @0x1a |
| i2c3  | i2c@30a50000  | **disabled** | — |

Note: the U-Boot LED controller (@0x53) and button GPIO expander (@0x20) referenced in `factory_reset`/`led_breath_white` are **not declared in the kernel DTB** — they are driven from userspace via i2c-dev (or a different bus numbering in U-Boot).

Other notable nodes: WiFi `brcm,bcm4329-fmac` on SDIO (usdhc); FEC ethernet @0x30be0000 w/ C22 PHY@4; CAAM crypto (`fsl,sec-v4.0`); MIPI-DSI + LCDIF display; CSI camera bridge; SAI audio; RPMsg/MU mailbox to the M4.

---

## 4. SWUpdate / OTA

Files: `/etc/swupdate.cfg`, `/etc/default/swupdate`, `/etc/swupdate_public.pem`, `/opt/eight/bin/swupdate.sh`, systemd `swupdate.service`.

- **OTA server: `https://update-api.8slp.net`** — endpoints `/v1/updates/p1/{0,1}`, progress `/v1/progress/{deviceid}`. `suricatta` polls every **3600 s**, `nocheckcert = true`.
- Hardware id: `imx8mm-var-dart-eight`. Device id example `p1a000000001`.
- **Update packages ARE signature-verified**: service runs `swupdate -k /etc/swupdate_public.pem` (RSA public key). `public-key-file` is commented out in `swupdate.cfg`, but the CLI `-k` flag is authoritative, so OTA `.swu` images must be signed by Eight's private key.
- This only gates **OTA**; it does not gate direct flashing of the eMMC/SD.

---

## 5. Assessment — installing fully custom FOSS firmware

**Bootloader is NOT locked; secure boot is NOT enforced.** The shipped SPL carries no HAB signature, so the SoC boots unsigned code. Nothing in the boot chain cryptographically constrains replacement.

Minimal set that must be **preserved / reused**:
- **DTB peripheral map** (or an equivalent DTS): the exact UART, I2C (PMIC bd71847 @0x4b, RTC rv3028, wm8904 codec), pinmux, WiFi (bcm4329), and M4/RPMsg wiring above must be reproduced or the board won't come up / power will be mis-sequenced. This is the single most important artifact to carry forward.
- The **i.MX8MM DDR training blobs + ATF (BL31) + DDR firmware** inside imx-boot are board-specific and should be reused (rebuild imx-boot for VAR-SOM-MX8M-MINI, or reuse Variscite's).

**Freely replaceable (no signing keys required):**
- SPL / U-Boot (can rebuild mainline or Variscite U-Boot; no CSF needed).
- Kernel `Image.gz` and rootfs (both slots).
- SWUpdate stack and the entire A/B scheme — can be dropped or replaced.

**Not required at all:** HAB signing keys / CSF / SRK (unused), Eight's SWUpdate private key (only needed to push signed OTAs; irrelevant for direct flashing).

**Recommended install path:** write a custom bootloader to raw offset 0x8000 and custom rootfs to p1/p2, keeping the DTB peripheral definitions. Serial console for bring-up is on **ttymxc3 @115200**. Keep p3 (`/cage`) if you want to retain persistent device state, or reformat it.

---

### Appendix — key offsets
- imx-boot / SPL IVT: file **0x8400** (`D1 00 20 41`), CSF ptr 0x0081C5C0 → file **0x43A00** = all zeros (unsigned).
- p1 ext4 @ 0x0080_0000 (label A) · p2 ext4 @ 0x1_869F_0000 (label B) · p3 ext4 @ 0x3_0D80_0000 (label cage).
- Parser + decompiled DTS kept at `.../work/dtb.dts`; `pod3.img` retained.
