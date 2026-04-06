#!/usr/bin/env bash
# E2E test: run syfrah CLI commands like a real user.
# Uses --mode mock to avoid WireGuard/TiKV dependencies.
# Exit 1 on any failure.
set -euo pipefail

SYFRAH="${SYFRAH_BIN:-syfrah}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

PASS=0
FAIL=0

check() {
    local name="$1"
    shift
    if "$@" > /dev/null 2>&1; then
        echo -e "  ${GREEN}✓${NC} $name"
        PASS=$((PASS + 1))
    else
        echo -e "  ${RED}✗${NC} $name"
        FAIL=$((FAIL + 1))
    fi
}

check_output() {
    local name="$1"
    local pattern="$2"
    shift 2
    local output
    output=$("$@" 2>&1) || true
    if echo "$output" | grep -q "$pattern"; then
        echo -e "  ${GREEN}✓${NC} $name"
        PASS=$((PASS + 1))
    else
        echo -e "  ${RED}✗${NC} $name (expected '$pattern' in output)"
        echo "    Got: $(echo "$output" | head -3)"
        FAIL=$((FAIL + 1))
    fi
}

cleanup() {
    # Kill any background syfrah processes
    pkill -x syfrah 2>/dev/null || true
    # Clean state
    echo y | $SYFRAH hypervisor leave 2>/dev/null || true
    rm -rf ~/.syfrah
}

echo "=== Syfrah CLI E2E Tests (mock mode) ==="
echo ""

# Cleanup before start
cleanup 2>/dev/null

# ── Init ──
echo "Init:"
check_output "init succeeds" "initialized" \
    $SYFRAH hypervisor init --region eu --zone test --mode mock

check_output "init detects already initialized" "already initialized" \
    $SYFRAH hypervisor init --region eu --zone test --mode mock

# ── Status ──
echo "Status:"
check_output "status shows name" "$(hostname | tr '[:upper:]' '[:lower:]')" \
    $SYFRAH hypervisor status

check_output "status shows region" "eu" \
    $SYFRAH hypervisor status

# ── List ──
echo "List:"
check_output "list shows this node" "$(hostname | tr '[:upper:]' '[:lower:]')" \
    $SYFRAH hypervisor list

# ── Doctor ──
echo "Doctor:"
check_output "doctor runs" "passed" \
    $SYFRAH hypervisor doctor

# ── API Server ──
echo "API Server:"
$SYFRAH serve --bind 127.0.0.1:18443 &
SERVE_PID=$!
sleep 2

check_output "health endpoint" '"status":"ok"' \
    curl -sf http://127.0.0.1:18443/health

check_output "list endpoint" '"items"' \
    curl -sf http://127.0.0.1:18443/admin/v1/hypervisor

check_output "openapi endpoint" '"openapi"' \
    curl -sf http://127.0.0.1:18443/openapi.json

kill $SERVE_PID 2>/dev/null
wait $SERVE_PID 2>/dev/null || true

# ── Stop / Start ──
echo "Lifecycle:"
check_output "stop succeeds" "stopped" \
    $SYFRAH hypervisor stop

check_output "start succeeds" "started" \
    $SYFRAH hypervisor start

# ── Leave ──
echo "Leave:"
check_output "leave succeeds" "uninstalled" \
    bash -c "echo y | $SYFRAH hypervisor leave"

check_output "status after leave fails" "not initialized" \
    $SYFRAH hypervisor status

# ── Help ──
echo "Help:"
check_output "help shows version" "2.0.0" \
    $SYFRAH --version

check_output "hypervisor help" "init" \
    $SYFRAH hypervisor --help

# ── Summary ──
echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

cleanup 2>/dev/null

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
