#!/bin/sh
# podd SD boot diagnostics. Writes to /opt/podd/bootlog (persistent ext4 on the
# SD rootfs). NOTE: /var/log is a tmpfs symlink on this image, so we must NOT log
# there. Read afterward on a host:  mount <sd>p1 /mnt; ls /mnt/opt/podd/bootlog
STAGE="${1:-late}"
OUT=/opt/podd/bootlog
mkdir -p "$OUT" 2>/dev/null
ts()   { date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo no-rtc; }
have() { command -v "$1" >/dev/null 2>&1; }
line() { echo "[$(ts)] up=$(cut -d' ' -f1 /proc/uptime 2>/dev/null) stage=$STAGE $*" >> "$OUT/timeline.txt" 2>/dev/null; }

line "enter"
dmesg > "$OUT/dmesg.$STAGE.txt" 2>/dev/null

snap() {
  tag="$1"
  { echo "===== snapshot $tag $(ts) ====="
    echo "-- uptime --";              cat /proc/uptime 2>&1
    echo "-- cmdline --";             cat /proc/cmdline 2>&1
    echo "-- failed units --";        systemctl --no-pager --failed 2>&1
    echo "-- net ifaces --";          ls -1 /sys/class/net 2>&1
    echo "-- ip addr --";             ip -o addr 2>&1
    echo "-- ip route --";            ip route 2>&1
    echo "-- /proc/net/wireless --";  cat /proc/net/wireless 2>&1
  } >> "$OUT/net.$STAGE.txt" 2>&1
  if have nmcli; then { echo "== nmcli $tag $(ts) =="; nmcli -t dev 2>&1; echo ---; nmcli -t con 2>&1; nmcli dev wifi 2>&1; } >> "$OUT/nmcli.txt" 2>&1; fi
  if have iw;    then { echo "== iw $tag $(ts) =="; iw dev 2>&1; for w in /sys/class/net/wl*; do [ -e "$w" ] && { echo "link $(basename "$w"):"; iw dev "$(basename "$w")" link 2>&1; }; done; } >> "$OUT/iw.txt" 2>&1; fi
}

if [ "$STAGE" = late ]; then
  i=0; while [ "$i" -lt 6 ]; do snap "t${i}"; sync; sleep 10; i=$((i+1)); done
  journalctl -b --no-pager -o short-precise            > "$OUT/journal.txt"       2>&1
  systemctl --no-pager list-units --state=failed       > "$OUT/failed.txt"        2>&1
  systemctl status podd.service --no-pager -l          > "$OUT/podd-status.txt"   2>&1
  lsmod                                                 > "$OUT/lsmod.txt"         2>&1
  lsmod | grep -iE 'cfg80211|mac80211|brcm|mwifiex|wilc|802|iwl|mt7|nvmem|regmap' > "$OUT/wifi-modules.txt" 2>&1
  cp /etc/fw_env.config "$OUT/" 2>/dev/null
  sync
else
  snap "$STAGE"; sync
fi
line "exit"
