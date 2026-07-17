# Eight Sleep Pod 4 / Pod 4 Ultra — FOSS Firmware Feasibility Research

**Date:** 2026-07-17
**Scope:** Web research only (no Pod 4 firmware in hand). Confidence tags: **[CONFIRMED]** = stated by a primary/technical source or FCC filing; **[INFERRED]** = strong indirect evidence; **[UNKNOWN]** = no public data found.

---

## TL;DR (go/no-go)

- **Boot is open.** The Pod 4 (and Pod 5) can be rooted with the *same* method as the Pod 3: connect a serial/JTAG adapter, interrupt U-Boot autoboot, `setenv bootargs ... init=/bin/bash`, boot to a root shell. This is documented and works today in `free-sleep`. **Secure boot / HAB / AHAB is NOT enforced** (an open U-Boot console that accepts modified boot args and boots an unsigned cmdline to a shell means no verified-boot chain). **This is a GO signal for custom firmware.** [CONFIRMED]
- **Boot medium is eMMC only** (no SD card on Pod 4, unlike early Pod 3). The blopker SD-card-swap root method does **not** apply; the serial/U-Boot method does. [CONFIRMED / INFERRED]
- **Compute platform is almost certainly still NXP i.MX8M-family on a Variscite-style SoM running Yocto Linux with A/B rootfs**, based on identical boot behavior and partition labels — but there is **no public Pod 4 board teardown** confirming the exact SoC part, RAM, or eMMC size. [INFERRED]
- **Root shell: achievable. Full open firmware: not yet.** `free-sleep` runs a LAN control server *on top of* Eight Sleep's stock firmware and supports Pod 4. `opensleep` (a full firmware replacement that talks directly to the STM32 MCUs) explicitly **does not** support Pod 4 — "untested, SSH possible but Pod-specific features not implemented." The blocker is that nobody has reverse-engineered the Pod 4 sensor/thermal MCU protocol, the new sensor suite, or the adjustable-base controller. [CONFIRMED]

---

## 1. SoC / compute platform

**Pod 3 baseline [CONFIRMED]:** Variscite VAR-SOM-MX8M-MINI (NXP i.MX8M Mini, quad Cortex-A53) running Yocto Linux; wifi/BT integrated on the SoM; boots from microSD (early units) or eMMC. (blopker ZeroSleep; opensleep README.)

**Pod 4:**
- **No public teardown of the Pod 4 compute module exists.** FCC internal photos / schematics / block diagram for the Pod 4 grant appear to be under confidentiality (the public FCC exhibit list shows only external photos, test-setup photos, antenna spec, RF/test reports, RF-exposure, label, and a **confidentiality request letter** — no internal photos, no block diagram). [CONFIRMED that internals are not public]
- **Strong indirect evidence the compute stack is unchanged in kind:** `free-sleep` and the adamschaal writeup treat Pod 4 identically to Pod 3 at the boot layer — same "Hit any key to stop autoboot" U-Boot prompt, same `root=PARTLABEL=rootfs_a` bootargs, same `root`/`rewt` accounts, same SSH on port 8822. That degree of parity almost only happens if Pod 4 is the same NXP i.MX + U-Boot + Yocto + A/B family. The install note that the Pod 4 login screen is "slightly different, that's OK" suggests a different OS build/hostname, not a different architecture. [INFERRED]
- **Eight Sleep's own Pod 4 Hub blog** says the redesigned Hub "hosts the connectivity and processing chips" but discloses no CPU/RAM/storage. A secondary search summary attributed a "quad-core CPU" to Eight Sleep marketing (consistent with i.MX8M Mini's quad A53), but I could not verify that phrase against primary text — treat as **[INFERRED/UNVERIFIED]**.
- **RAM / eMMC size:** **[UNKNOWN]** publicly for Pod 4.
- **WiFi/BT module:** FCC test report confirms dual-band operation — 2.4 GHz (2402–2480 / 2412–2462) and 5 GHz UNII bands (5180–5240, 5745–5825), i.e. 802.11 a/b/g/n/ac-class, plus BT. The **specific module make/model is not disclosed** (test report is image-based; internal photos confidential). On Variscite i.MX8M-Mini SoMs this is typically an onboard Murata/NXP combo module — **[INFERRED]**, not confirmed for Pod 4. Grantee: **Eight Sleep Inc, FCC ID 2AYXT61100002, model 10504**, grant/test dated March 2024. [CONFIRMED]

## 2. Boot process (the critical go/no-go)

- **Bootloader: U-Boot, and its console is open.** Root procedure interrupts autoboot ("Hit any key to stop autoboot", CTRL+C) into the U-Boot shell. [CONFIRMED, applies to Pod 4/5]
- **Secure boot: not enforced.** From the U-Boot shell you can `setenv bootargs = root=PARTLABEL=rootfs_a rootwait init=/bin/bash` and boot straight to a root shell, then remount rootfs rw and change passwords. A working `init=/bin/bash` + modified bootargs path means the kernel command line and rootfs are **not signature-verified** — i.e., NXP HAB/AHAB verified boot is either not fused/enabled or not enforced on the payload. **This is the single most important finding: Pod 4 is not locked down against custom boot, same as Pod 3.** [CONFIRMED via free-sleep/adamschaal, INFERRED that HAB is off]
- **Boot medium:** eMMC. Pod 4 has **no SD card** (free-sleep lists "Pod 3 (No SD card)", "Pod 4", "Pod 5" together as the eMMC-only, serial-rooted class). "Pod 4 ... impossible to brick as long as you follow the directions" — because there's no removable card to corrupt and the reset/recovery path restores the stock image. [CONFIRMED]
- **Serial access:** FTDI FT232RL (or TC2070-IDC Tag-Connect) to the board's JTAG/serial header, **921600 baud**, minicom/screen. free-sleep ships JTAG connection photos in `docs/jtag/`. [CONFIRMED]

## 3. Partition / OS layout

- **A/B rootfs confirmed still present:** the generic (Pod 3/4/5) instructions reference `PARTLABEL=rootfs_a` and require verifying `current_slot = a` (firmware-reset to slot A if not). [CONFIRMED]
- **Yocto:** **[INFERRED]** still Yocto (same toolchain/stack as Pod 3; no evidence of change).
- **Update mechanism:** Pod 3 uses **SWUpdate** consuming `.swu` packages pulled from Eight Sleep cloud (`*.8slp.net`) via curl. For Pod 4, the root procedure **disables the OTA/update service via systemctl** to prevent the stock firmware from reverting the jailbreak — which implies the **same SWUpdate-based OTA design carries over**. Exact Pod 4 endpoints/package format **[UNKNOWN/INFERRED]**.

## 4. Hardware control architecture

- **Pod 3 baseline [CONFIRMED]:** the i.MX8 Linux SoC ("Frank"/master) talks over **UART to two STM32 microcontrollers** — **"Sensor"** (temperature, capacitive presence, piezoelectric, vibration motors) and **"Frozen"** (water pump, priming, thermoelectric heat/cool). (opensleep.)
- **Pod 4:** **[UNKNOWN at the silicon level].** No teardown identifies the Pod 4 MCUs or confirms it still uses two STM32s over UART. The fact that `opensleep` says Pod-4 "Pod-specific features not implemented" strongly implies the **MCU set and/or serial protocol differ** enough that the Pod 3 daemon can't just be pointed at Pod 4. [INFERRED]
- **Sensors (Pod 4 marketing):** ~**36 sensors**; snoring detected via **body vibration (piezo), not a microphone**; heart rate, HRV, respiratory rate, sleep stages. Sensor suite is revamped vs Pod 3 → **new firmware/calibration blobs to RE**. [CONFIRMED marketing / INFERRED impact]
- **Adjustable base (Pod 4 Ultra only):** a motorized base providing Reading/Sleeping positions and automatic anti-snore articulation, driven by the Hub. **Control interface unknown** — likely an additional motor/actuator controller (its own MCU) commanded by the Hub; not publicly documented. [UNKNOWN]

## 5. Root / RE status

- **Rooted? Yes — shell access is a solved problem on Pod 4.** `free-sleep` officially lists **Pod 4 ✅ and Pod 5 ✅**, using the serial/U-Boot method above; the adamschaal (Dec 2025) writeup independently confirms "Pod 3 & Pod 4 ... compatible." You get a root shell, disable OTA, join wifi, and run free-sleep's Node.js LAN server for temperature/schedule/alarm control. [CONFIRMED]
- **But that's control-on-top-of-stock-firmware, not a firmware replacement.** `free-sleep` rides Eight Sleep's existing daemons; it does not replace the vendor stack or independently drive the MCUs.
- **Full open firmware? Not for Pod 4.** `opensleep` (which *replaces* the Eight Sleep programs and speaks directly to the STM32s) states Pod 4/5 are **"Untested. SSH setup possible but Pod-specific features not implemented,"** with an open call for contributors. [CONFIRMED]
- **What's blocking Pod 4 full support:** (a) no public dump/teardown of the Pod 4 compute board (exact SoC, eMMC layout, MCU part numbers); (b) the sensor/thermal MCU protocol appears changed vs Pod 3 and hasn't been reverse-engineered; (c) new sensor hardware + the adjustable-base controller are entirely undocumented. Getting a **shell is not the blocker** — RE of the real-time control plane is. [INFERRED]
- **Pod 5 note:** must be set up in the Eight Sleep app *before* jailbreaking — a sign the newest generation is somewhat more cloud-coupled at provisioning. Not confirmed for Pod 4. [CONFIRMED for Pod 5]

## 6. Differences that matter for a replacement

- **Bootloader appears unlocked (GO):** open U-Boot, no enforced secure boot — the decisive factor, and it favors a custom-firmware project.
- **eMMC-only, no SD:** recovery differs from early Pod 3 (no card swap), but factory reset still restores a known image, so experimentation is low-risk ("impossible to brick").
- **New blobs / new control plane:** revamped sensor suite, snore detection, and (Ultra) adjustable base = new MCU firmware and protocols that must be reverse-engineered before an opensleep-style takeover works.
- **Possibly more cloud coupling on newest gen** (Pod 5 app-provisioning requirement); unverified for Pod 4.
- **FCC internals confidential:** you cannot shortcut SoC/module ID via the FCC database for Pod 4 — a physical teardown is required.

## 7. How generalizable is a Pod-3-based replacement approach to Pod 4? (honest assessment)

Two layers, two different answers:

- **Access / boot layer — HIGHLY generalizable.** The Pod-3 privilege-escalation model (open U-Boot, no secure boot, A/B Yocto rootfs, serial console) transfers cleanly to Pod 4. Root is already reproducible in the wild via free-sleep. If your goal is "get a shell and run my own userspace on the existing Linux," Pod 4 is basically as open as Pod 3.
- **Hardware-control / true-FOSS-firmware layer — NOT yet generalizable.** An opensleep-style replacement that owns the thermal loop and sensors depends on the specific STM32 UART protocol, sensor calibration, and (for Ultra) base actuator control — all of which are Pod-3-specific and **have not been reverse-engineered for Pod 4**. Expect real work: a teardown to ID the board/MCUs, logic capture of the master↔MCU UART, and protocol RE. That effort is plausible (the platform is open and the community is active) but it is **greenfield for Pod 4** today.

**Bottom line:** Pod 4 is an open, rootable target — the go/no-go (secure boot) lands on **GO**. A Pod-3-style *rooting* approach generalizes directly; a Pod-3-style *full open firmware* does not yet, and standing one up for Pod 4 requires new reverse-engineering of the sensor/thermal MCU stack and the adjustable base. Key unknowns to close with a physical unit: exact SoC/RAM/eMMC, WiFi/BT module, the Pod 4 MCU part numbers and UART protocol, and how the adjustable base is driven.

---

## Sources

- blopker, "ZeroSleep: Eight Sleep Pod 3 root access" — https://blopker.com/writing/04-zerosleep-1/ (Pod 3 SoC = Variscite VAR-SOM-MX8M-MINI, Yocto, SWUpdate/.swu, SD-card method; no Pod 4)
- Adam Schaal, "Rooting My Eight Sleep Pod 3" (Dec 2025) — https://blog.adamschaal.com/posts/2025-12-16-rooting-eight-sleep/ ("Pod 3 & Pod 4 compatible"; JTAG/U-Boot method; disable OTA)
- free-sleep (throwaway31265) README — https://github.com/throwaway31265/free-sleep (compatibility: Pod 3 no-SD/Pod 4/Pod 5 ✅; "impossible to brick"; FCC ID 2AYXT61100001 for Pod 3)
- free-sleep INSTALLATION.md — https://github.com/throwaway31265/free-sleep/blob/main/INSTALLATION.md (921600 baud, U-Boot autoboot interrupt, `bootargs ... init=/bin/bash`, `PARTLABEL=rootfs_a`, `current_slot=a`, Pod 4/5 login "slightly different", Pod 5 app-setup-first, `docs/jtag/` photos)
- opensleep (LiamSnow) README — https://github.com/LiamSnow/opensleep (Pod 3 architecture: i.MX8M Mini master + two STM32s "Sensor"/"Frozen" over UART; "Pod 4/5: Untested. SSH setup possible but Pod-specific features not implemented")
- opensleep project page — https://liamsnow.com/projects/opensleep/
- FCC ID 2AYXT61100002 (Pod 4, Eight Sleep Inc, model 10504) exhibit list — https://fcc.report/FCC-ID/2AYXT61100002 (external photos, test-setup photos, antenna spec, RF/test reports, RF-exposure, label, confidentiality-request letter; NO public internal photos/block diagram; dual-band 2.4/5 GHz)
- FCC Pod 4 RF test report PDF — https://fcc.report/FCC-ID/2AYXT61100002/7169183.pdf (image-based; module make/model not extractable)
- Eight Sleep Inc FCC filings index — https://fcc.report/company/Eight-Sleep-Inc
- Eight Sleep, "Behind the Design: Pod 4 Hub" — https://www.eightsleep.com/blog/pod-4-hub/ ("connectivity and processing chips"; solid-state heat pumps, dual fans, 30 dB, 2x cooling; no CPU disclosure)
- Eight Sleep Pod 4 Ultra press release (Business Wire, 2024-05-08) — https://www.businesswire.com/news/home/20240508677795/en/ (36 sensors, vibration-based snore detection, adjustable base)
- Sleep Review, Pod 4 Ultra snoring algorithm — https://sleepreviewmag.com/.../eight-sleeps-pod-4-ultra-tackles-snoring-with-new-detection-algorithm/
- Tom's Hardware / LTT — Eight Sleep SSH backdoor coverage (context on stock firmware remote access) — https://www.tomshardware.com/tech-industry/cyber-security/security-researcher-finds-vulnerability-in-internet-connected-bed-could-allow-access-to-all-devices-on-network
