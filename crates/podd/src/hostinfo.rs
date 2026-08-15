//! Host facts for `/api/deviceStatus`: hub board identity + WiFi strength.
//!
//! These are Linux-host properties (device tree, `/proc/net/wireless`), not MCU
//! telemetry, so they are read here in the binary rather than in `podd-core`.
//! On a dev box neither source exists and the store keeps its hide-the-chip
//! defaults ("Version not found" / 0).

use api::StateStore;
use std::sync::Arc;
use std::time::Duration;

const WIFI_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Detect the hub once and keep the WiFi strength fresh.
pub fn spawn(store: Arc<StateStore>) {
    tokio::spawn(async move {
        if let Some(hub) = detect_hub_version() {
            log::info!("host info: hub identified as {hub}");
            store.set_hub_version(hub);
        }
        loop {
            if let Some(percent) = read_wifi_strength() {
                store.set_wifi_strength(percent);
            }
            tokio::time::sleep(WIFI_POLL_INTERVAL).await;
        }
    });
}

/// Map the device-tree model to a hub generation.
///
/// Pod 3 "SD" and Pod 4 hubs are both i.MX8M Mini boards (docs/FLASHING.md
/// "hub" table); what separates them is the SD card, so the SoC match is
/// refined by whether an SD device is present. The Eight Sleep app's
/// `coverVersion` says nothing about the hub — this is a board fact.
fn detect_hub_version() -> Option<String> {
    let model = std::fs::read_to_string("/proc/device-tree/model").ok()?;
    Some(hub_version_from(&model, host_has_sd_card()))
}

fn hub_version_from(dt_model: &str, has_sd_card: bool) -> String {
    let model = dt_model.to_lowercase();
    if model.contains("mt8365") || model.contains("mediatek") {
        "MediaTek".to_string()
    } else if model.contains("mx8m") {
        if has_sd_card { "Pod 3" } else { "Pod 4" }.to_string()
    } else {
        "Version not found".to_string()
    }
}

/// True if any MMC block device is an SD card (`/sys/block/mmcblk*` with
/// `device/type` == "SD"; eMMC reports "MMC").
fn host_has_sd_card() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/block") else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name().to_string_lossy().starts_with("mmcblk")
            && std::fs::read_to_string(e.path().join("device/type"))
                .is_ok_and(|t| t.trim() == "SD")
    })
}

fn read_wifi_strength() -> Option<i32> {
    let contents = std::fs::read_to_string("/proc/net/wireless").ok()?;
    parse_wireless_quality(&contents)
}

/// Parse `/proc/net/wireless` into a 0–100 percentage.
///
/// The "Quality link" column is on a 0–70 scale under cfg80211 drivers.
/// Returns the first interface's value; the Pod has a single wlan.
fn parse_wireless_quality(contents: &str) -> Option<i32> {
    let line = contents.lines().skip(2).find(|l| l.contains(':'))?;
    let after_iface = line.split(':').nth(1)?;
    // columns: status, link quality, level, noise, ...
    let quality: f64 = after_iface
        .split_whitespace()
        .nth(1)?
        .trim_end_matches('.')
        .parse()
        .ok()?;
    Some(((quality / 70.0 * 100.0).round() as i32).clamp(0, 100))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIRELESS: &str = "\
Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE
 face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22
 wlan0: 0000   54.  -56.  -256        0      0      0      0      0        0
";

    #[test]
    fn parses_wireless_quality() {
        assert_eq!(parse_wireless_quality(WIRELESS), Some(77));
    }

    #[test]
    fn no_interface_line_is_none() {
        let header_only: String = WIRELESS.lines().take(2).collect::<Vec<_>>().join("\n");
        assert_eq!(parse_wireless_quality(&header_only), None);
    }

    #[test]
    fn hub_mapping() {
        let podd_dt = "Eight Sleep Pod (New-Rat 0.8) on Variscite DART-MX8M-MINI";
        assert_eq!(hub_version_from(podd_dt, true), "Pod 3");
        assert_eq!(hub_version_from(podd_dt, false), "Pod 4");
        assert_eq!(
            hub_version_from("MediaTek MT8365 Genio 350", false),
            "MediaTek"
        );
        assert_eq!(hub_version_from("Some Other Board", true), "Version not found");
    }
}
