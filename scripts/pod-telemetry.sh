#!/usr/bin/env bash
#
# pod-telemetry.sh - print a Pod's telemetry-recorder lines (docs/ARCHITECTURE.md,
# "Telemetry recorder") for one or more UTC days, raw or gzipped, to stdout.
#
#   scripts/pod-telemetry.sh [-p port] [-i key] root@<pod> [YYYY-MM-DD ...]
#
# No day = today. Pipe into jq, e.g. the right side through a night:
#   scripts/pod-telemetry.sh root@pod 2026-10-18 \
#     | jq -c 'select(.kind=="sample") | [.ts, .right.water, .right.eff, .right.mcu, .fw.pump.right.a]'
set -euo pipefail

port=22
key=()
while getopts "p:i:" opt; do
  case $opt in
    p) port=$OPTARG ;;
    i) key=(-i "$OPTARG") ;;
    *) exit 2 ;;
  esac
done
shift $((OPTIND - 1))
host=${1:?usage: $0 [-p port] [-i key] user@host [YYYY-MM-DD ...]}
shift
days=("$@")
if [ ${#days[@]} -eq 0 ]; then
  days=("$(date -u +%F)")
fi

for day in "${days[@]}"; do
  case $day in
    [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
    *) echo "bad day: $day" >&2; exit 2 ;;
  esac
  # the day is either still raw (today) or already gzipped
  ssh "${key[@]}" -p "$port" "$host" \
    "d=/data/podd/telemetry; if [ -f \$d/$day.jsonl.gz ]; then zcat \$d/$day.jsonl.gz; elif [ -f \$d/$day.jsonl ]; then cat \$d/$day.jsonl; else echo 'no telemetry for $day' >&2; exit 1; fi"
done
