#!/usr/bin/env bash
set -euo pipefail

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info() {
  echo -e "${YELLOW}[INFO] $*${NC}"
}

ok() {
  echo -e "${GREEN}[OK] $*${NC}"
}

fail() {
  echo -e "${RED}[ERROR] $*${NC}" >&2
}

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${ROOT_DIR}"

PCAP_FILE="quic_traffic.pcap"
TICKET_FILE="session.ticket"
RECV1_LOG="recv_1.log"
RECV2_LOG="recv_2.log"
SEND2_LOG="send_2.log"

TCPDUMP_PID=""
RECV1_PID=""
RECV2_PID=""

cleanup() {
  set +e
  if [[ -n "${TCPDUMP_PID}" ]] && kill -0 "${TCPDUMP_PID}" 2>/dev/null; then
    kill -INT "${TCPDUMP_PID}" 2>/dev/null || true
    wait "${TCPDUMP_PID}" 2>/dev/null || true
  fi
  if [[ -n "${RECV1_PID}" ]] && kill -0 "${RECV1_PID}" 2>/dev/null; then
    kill "${RECV1_PID}" 2>/dev/null || true
    wait "${RECV1_PID}" 2>/dev/null || true
  fi
  if [[ -n "${RECV2_PID}" ]] && kill -0 "${RECV2_PID}" 2>/dev/null; then
    kill "${RECV2_PID}" 2>/dev/null || true
    wait "${RECV2_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

detect_loopback_iface() {
  if command -v ifconfig >/dev/null 2>&1; then
    if ifconfig lo0 >/dev/null 2>&1; then
      echo "lo0"
      return 0
    fi
    if ifconfig lo >/dev/null 2>&1; then
      echo "lo"
      return 0
    fi
  fi
  if command -v ip >/dev/null 2>&1 && ip link show lo >/dev/null 2>&1; then
    echo "lo"
    return 0
  fi
  return 1
}

wait_for_receiver() {
  local pid="$1"
  local label="$2"
  local timeout_s="$3"
  local waited=0
  while kill -0 "${pid}" 2>/dev/null; do
    if (( waited >= timeout_s )); then
      fail "${label} timed out after ${timeout_s}s"
      return 1
    fi
    sleep 1
    waited=$((waited + 1))
  done
  wait "${pid}"
}

info "==== Phase 1: Codebase & Dependency Check ===="
cargo tree -p sankaku-core | grep quinn
ok "Found quinn in dependency tree."

if cargo tree -p sankaku-core | grep -E "chacha20poly1305|x25519-dalek" >/dev/null; then
  fail "Legacy crypto crates still present in dependency tree."
  cargo tree -p sankaku-core | grep -E "chacha20poly1305|x25519-dalek" || true
  exit 1
fi
ok "Legacy crypto crates removed: chacha20poly1305 and x25519-dalek not found."

info "==== Phase 2: Functional 1-RTT & Traffic Capture (MTU Test) ===="
LO_IFACE="$(detect_loopback_iface)" || {
  fail "Could not detect loopback interface (expected lo or lo0)."
  exit 1
}
info "Using loopback interface: ${LO_IFACE}"

rm -f "${PCAP_FILE}" "${TICKET_FILE}" "${RECV1_LOG}" "${RECV2_LOG}" "${SEND2_LOG}"

tcpdump -i "${LO_IFACE}" -n udp port 8080 -w "${PCAP_FILE}" >/dev/null 2>&1 &
TCPDUMP_PID=$!
sleep 1
if ! kill -0 "${TCPDUMP_PID}" 2>/dev/null; then
  fail "tcpdump failed to start. Try running with permissions that allow packet capture."
  exit 1
fi
ok "tcpdump started in background (pid=${TCPDUMP_PID})."

cargo run --release -p sankaku-cli -- recv --bind 127.0.0.1:8080 --max-frames 120 > "${RECV1_LOG}" 2>&1 &
RECV1_PID=$!
sleep 1
if ! kill -0 "${RECV1_PID}" 2>/dev/null; then
  fail "Phase 2 receiver exited early. See ${RECV1_LOG}"
  exit 1
fi

cargo run --release -p sankaku-cli -- send --dest 127.0.0.1:8080 --frames 120 --payload-bytes 1300 --ticket-out "${TICKET_FILE}"
wait_for_receiver "${RECV1_PID}" "Phase 2 receiver" 120

if [[ -n "${TCPDUMP_PID}" ]] && kill -0 "${TCPDUMP_PID}" 2>/dev/null; then
  kill -INT "${TCPDUMP_PID}" 2>/dev/null || true
  wait "${TCPDUMP_PID}" 2>/dev/null || true
fi
TCPDUMP_PID=""
ok "Phase 2 complete. Traffic capture written to ${PCAP_FILE}"

info "==== Phase 3: Functional 0-RTT Resumption & Miniature Benchmark ===="
cargo run --release -p sankaku-cli -- recv --bind 127.0.0.1:8080 --max-frames 500 > "${RECV2_LOG}" 2>&1 &
RECV2_PID=$!
sleep 1
if ! kill -0 "${RECV2_PID}" 2>/dev/null; then
  fail "Phase 3 receiver exited early. See ${RECV2_LOG}"
  exit 1
fi

{
  time cargo run --release -p sankaku-cli -- send --dest 127.0.0.1:8080 --frames 500 --fps 1000 --payload-bytes 1200 --ticket-in "${TICKET_FILE}"
} > "${SEND2_LOG}" 2>&1
wait_for_receiver "${RECV2_PID}" "Phase 3 receiver" 240
ok "Phase 3 benchmark complete."

info "==== Phase 4: Telemetry & Cleanup ===="
grep -F "QUIC_TELEMETRY: path.rtt=" "${SEND2_LOG}" >/dev/null
ok "Found QUIC telemetry marker in ${SEND2_LOG}."

rm -f "${TICKET_FILE}"
ok "Removed ${TICKET_FILE}."

ok "Verification succeeded. Open ${PCAP_FILE} in Wireshark and confirm QUIC v1/TLS 1.3 traffic."
info "Logs kept for inspection: ${RECV1_LOG}, ${RECV2_LOG}, ${SEND2_LOG}"
