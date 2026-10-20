#!/bin/sh
#
# stress-test.sh - daytime thermal characterization of a Pod, run ON the Pod
# (busybox sh + wget) against podd's local API, on an EMPTY bed.
#
#   nohup sh /data/podd/stress-test.sh > /dev/null 2>&1 &
#
# Steps each side alone through heat, cool and deep-cool targets, then cools
# both at once (they share the TEC heatsink). Everything podd sees is in the
# telemetry recorder (10 s samples); this script only sets targets and marks
# the phases in the journal (`journalctl -t podd-stress`) and in
# /data/podd/stress-<UTC date>.log, so the samples can be cut by phase.
#
# Safety: the freeze guard stays in charge of every cooling target (podd
# ramps, detects and thaws). The highest target is 95 °F (35 °C). If anyone
# gets on the bed — or the presence endpoint stops answering — both sides go
# off and the script exits. Make sure the daily prime is not due before it
# finishes (about 5 h 10 min).

API=http://127.0.0.1:3000/api
LOG=/data/podd/stress-$(date -u +%F).log
POLL=30

say() {
    echo "$(date -u +%FT%TZ) $*" >> "$LOG"
    logger -t podd-stress "$*"
}

post() {
    wget -q -O /dev/null --header 'Content-Type: application/json' \
        --post-data "$1" "$API/deviceStatus"
}

both_off() {
    post '{"left":{"isOn":false},"right":{"isOn":false}}'
}

abort() {
    both_off
    say "ABORT: $1 - both sides off"
    exit 1
}

# Wait $1 minutes, checking presence every $POLL s.
hold() {
    end=$(( $(date +%s) + $1 * 60 ))
    while [ "$(date +%s)" -lt "$end" ]; do
        p=$(wget -q -O - "$API/metrics/presence" 2>/dev/null) || abort "presence endpoint not answering"
        case $p in
            *'"present":true'*) abort "someone is on the bed" ;;
        esac
        sleep "$POLL"
    done
}

# phase <name> <minutes> <json>
phase() {
    say "PHASE $1 ($2 min): $3"
    [ -n "$3" ] && post "$3"
    hold "$2"
}

trap 'abort "interrupted"' INT TERM

say "START stress test"
both_off
phase baseline 10 ''

for side in right left; do
    phase "$side-heat-95F" 30 "{\"$side\":{\"targetTemperatureF\":95}}"
    phase "$side-cool-68F" 45 "{\"$side\":{\"targetTemperatureF\":68}}"
    phase "$side-deep-60F" 45 "{\"$side\":{\"targetTemperatureF\":60}}"
    # thaw: a warm target keeps the pump running while the TEC stops cooling
    phase "$side-thaw-86F" 10 "{\"$side\":{\"targetTemperatureF\":86}}"
    phase "$side-off" 0 "{\"$side\":{\"isOn\":false}}"
done

phase both-cool-68F 40 '{"left":{"targetTemperatureF":68},"right":{"targetTemperatureF":68}}'
both_off
say "DONE stress test - both sides off"
