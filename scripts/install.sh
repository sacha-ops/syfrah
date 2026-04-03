#!/bin/sh
# Install script for syfrah — downloads the latest release binary.
set -eu

REPO="sacha-ops/syfrah"
BIN="syfrah"
INSTALL_DIR="/usr/local/bin"
CHANNEL="stable"

# Parse arguments
for arg in "$@"; do
  case "$arg" in
    --beta) CHANNEL="beta" ;;
  esac
done

# --- UX helpers -----------------------------------------------------------

# Unicode symbols for step feedback
CHECK="\342\234\223"   # ✓
CROSS="\342\234\227"   # ✗
ARROW="\342\206\223"   # ↓

# Spinner characters (braille dots — renders in virtually every modern terminal)
SPINNER_CHARS='|/-\'

step_ok() {
  printf "  %b %s\n" "$CHECK" "$1"
}

step_fail() {
  printf "  %b %s\n" "$CROSS" "$1" >&2
}

# Start a background spinner. Usage: start_spinner "message"
# Sets SPINNER_PID for later use by stop_spinner.
SPINNER_PID=""
start_spinner() {
  _msg="$1"
  (
    i=0
    while true; do
      c=$(printf '%s' "$SPINNER_CHARS" | cut -c$(( (i % 4) + 1 )))
      printf "\r  %s %s" "$c" "$_msg"
      i=$(( i + 1 ))
      sleep 0.15 2>/dev/null || sleep 1
    done
  ) &
  SPINNER_PID=$!
}

# Stop the spinner and print a final status line.
# Usage: stop_spinner "done message" [ok|fail]
stop_spinner() {
  _final_msg="$1"
  _status="${2:-ok}"
  if [ -n "$SPINNER_PID" ]; then
    kill "$SPINNER_PID" 2>/dev/null || true
    wait "$SPINNER_PID" 2>/dev/null || true
    SPINNER_PID=""
  fi
  # Clear the spinner line
  printf "\r                                                                \r"
  if [ "$_status" = "ok" ]; then
    step_ok "$_final_msg"
  else
    step_fail "$_final_msg"
  fi
}

# Ensure spinner is cleaned up on exit
cleanup() {
  if [ -n "$SPINNER_PID" ]; then
    kill "$SPINNER_PID" 2>/dev/null || true
    wait "$SPINNER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# --- Detect OS -------------------------------------------------------------

OS="$(uname -s)"
case "$OS" in
  Linux)  OS="unknown-linux-musl" ;;
  Darwin) OS="apple-darwin" ;;
  *)
    step_fail "Unsupported operating system: $OS"
    exit 1
    ;;
esac

# --- Detect architecture ---------------------------------------------------

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64)             ARCH="x86_64" ;;
  aarch64|arm64)      ARCH="aarch64" ;;
  *)
    step_fail "Unsupported architecture: $ARCH"
    exit 1
    ;;
esac

TARGET="${ARCH}-${OS}"

# --- Fetch release tag -----------------------------------------------------

if [ "$CHANNEL" = "beta" ]; then
  start_spinner "Fetching latest beta release version..."
  # Beta releases are pre-releases — /releases/latest ignores them.
  # List all releases, find tags containing "beta" (most recent first).
  VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases?per_page=20" \
    | grep '"tag_name"' | grep 'beta' | head -1 \
    | sed 's/.*"tag_name": *"//;s/".*//')" || true
  if [ -z "$VERSION" ]; then
    stop_spinner "No beta release found" fail
    exit 1
  fi
  stop_spinner "Latest beta: ${VERSION}"
else
  start_spinner "Fetching latest stable release version..."
  VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')" || true
  if [ -z "$VERSION" ]; then
    stop_spinner "Could not determine latest release version" fail
    exit 1
  fi
  stop_spinner "Latest version: ${VERSION}"
fi

# --- Download archive -------------------------------------------------------

ARCHIVE="${BIN}-${VERSION}-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"

TMPDIR="$(mktemp -d)"
# Override the cleanup trap to also remove TMPDIR
trap 'cleanup; rm -rf "$TMPDIR"' EXIT

start_spinner "Downloading ${BIN} ${VERSION}..."
if curl -fsSL -o "${TMPDIR}/${ARCHIVE}" "$URL"; then
  # Get file size for display
  if [ -f "${TMPDIR}/${ARCHIVE}" ]; then
    SIZE=$(wc -c < "${TMPDIR}/${ARCHIVE}" | tr -d ' ')
    SIZE_MB=$(( SIZE / 1048576 ))
    if [ "$SIZE_MB" -gt 0 ]; then
      stop_spinner "Downloaded ${BIN} ${VERSION} (${SIZE_MB} MB)"
    else
      SIZE_KB=$(( SIZE / 1024 ))
      stop_spinner "Downloaded ${BIN} ${VERSION} (${SIZE_KB} KB)"
    fi
  else
    stop_spinner "Downloaded ${BIN} ${VERSION}"
  fi
else
  stop_spinner "Failed to download ${URL}" fail
  exit 1
fi

# --- Verify checksum -------------------------------------------------------

CHECKSUMS_URL="https://github.com/${REPO}/releases/download/${VERSION}/SHA256SUMS.txt"
start_spinner "Verifying checksum..."

if ! curl -fsSL -o "${TMPDIR}/SHA256SUMS.txt" "$CHECKSUMS_URL"; then
  stop_spinner "Could not download SHA256SUMS.txt — verify the release manually" fail
  exit 1
fi

EXPECTED="$(grep -F "${ARCHIVE}" "${TMPDIR}/SHA256SUMS.txt" | head -1 | awk '{print $1}')"
if [ -z "$EXPECTED" ]; then
  stop_spinner "No checksum found for ${ARCHIVE} in SHA256SUMS.txt" fail
  exit 1
fi

if command -v sha256sum > /dev/null 2>&1; then
  ACTUAL="$(sha256sum "${TMPDIR}/${ARCHIVE}" | awk '{print $1}')"
elif command -v shasum > /dev/null 2>&1; then
  ACTUAL="$(shasum -a 256 "${TMPDIR}/${ARCHIVE}" | awk '{print $1}')"
else
  stop_spinner "No sha256sum or shasum command found" fail
  exit 1
fi

if [ "$EXPECTED" != "$ACTUAL" ]; then
  stop_spinner "Checksum mismatch for ${ARCHIVE}" fail
  printf "    expected: %s\n" "$EXPECTED" >&2
  printf "    actual:   %s\n" "$ACTUAL" >&2
  exit 1
fi

stop_spinner "Checksum verified"

# --- Extract and install ----------------------------------------------------

start_spinner "Extracting binary..."
if tar xzf "${TMPDIR}/${ARCHIVE}" -C "$TMPDIR"; then
  stop_spinner "Extracted binary"
else
  stop_spinner "Failed to extract ${ARCHIVE}" fail
  exit 1
fi

start_spinner "Installing to ${INSTALL_DIR}/${BIN}..."
if install -m 755 "${TMPDIR}/${BIN}" "${INSTALL_DIR}/${BIN}"; then
  stop_spinner "Installed to ${INSTALL_DIR}/${BIN}"
else
  stop_spinner "Failed to install ${BIN} to ${INSTALL_DIR} (are you root?)" fail
  exit 1
fi

# --- Install Cloud Hypervisor (if bundled) ----------------------------------

CH_BIN="cloud-hypervisor"
CH_INSTALL_DIR="/usr/local/lib/syfrah"

if [ -f "${TMPDIR}/${CH_BIN}" ]; then
  start_spinner "Installing Cloud Hypervisor to ${CH_INSTALL_DIR}/${CH_BIN}..."
  mkdir -p "$CH_INSTALL_DIR"
  if install -m 755 "${TMPDIR}/${CH_BIN}" "${CH_INSTALL_DIR}/${CH_BIN}"; then
    stop_spinner "Installed Cloud Hypervisor to ${CH_INSTALL_DIR}/${CH_BIN}"
  else
    stop_spinner "Failed to install Cloud Hypervisor (are you root?)" fail
    exit 1
  fi
fi

# --- Install ZeroFS (if bundled) --------------------------------------------

ZEROFS_BIN="zerofs"
ZEROFS_INSTALL_DIR="/usr/local/lib/syfrah"

if [ -f "${TMPDIR}/${ZEROFS_BIN}" ]; then
  start_spinner "Installing ZeroFS to ${ZEROFS_INSTALL_DIR}/${ZEROFS_BIN}..."
  mkdir -p "$ZEROFS_INSTALL_DIR"
  if install -m 755 "${TMPDIR}/${ZEROFS_BIN}" "${ZEROFS_INSTALL_DIR}/${ZEROFS_BIN}"; then
    stop_spinner "Installed ZeroFS to ${ZEROFS_INSTALL_DIR}/${ZEROFS_BIN}"
  else
    stop_spinner "Failed to install ZeroFS (are you root?)" fail
    exit 1
  fi
fi

# --- Install kernel (if bundled) -------------------------------------------

KERNEL_BIN="vmlinux"
KERNEL_INSTALL_DIR="/opt/syfrah/kernels"

if [ -f "${TMPDIR}/${KERNEL_BIN}" ]; then
  start_spinner "Installing kernel to ${KERNEL_INSTALL_DIR}/${KERNEL_BIN}..."
  mkdir -p "$KERNEL_INSTALL_DIR"
  if install -m 644 "${TMPDIR}/${KERNEL_BIN}" "${KERNEL_INSTALL_DIR}/${KERNEL_BIN}"; then
    stop_spinner "Installed kernel to ${KERNEL_INSTALL_DIR}/${KERNEL_BIN}"
  else
    stop_spinner "Failed to install kernel (are you root?)" fail
    exit 1
  fi
fi

# --- Install crun (if bundled) -----------------------------------------------

CRUN_BIN="crun"
if [ -f "${TMPDIR}/${CRUN_BIN}" ]; then
  start_spinner "Installing crun to ${INSTALL_DIR}/${CRUN_BIN}..."
  if install -m 755 "${TMPDIR}/${CRUN_BIN}" "${INSTALL_DIR}/${CRUN_BIN}"; then
    stop_spinner "Installed crun to ${INSTALL_DIR}/${CRUN_BIN}"
  else
    stop_spinner "Failed to install crun (are you root?)" fail
    exit 1
  fi
else
  # Fallback: download crun if not bundled (older release)
  if ! command -v crun > /dev/null 2>&1; then
    CRUN_VERSION="${SYFRAH_CRUN_VERSION:-1.18.2}"
    case "$ARCH" in
      x86_64)  CRUN_ARCH="amd64" ;;
      aarch64) CRUN_ARCH="arm64" ;;
      *)
        step_fail "Unsupported architecture for crun: $ARCH"
        exit 1
        ;;
    esac
    CRUN_URL="https://github.com/containers/crun/releases/download/${CRUN_VERSION}/crun-${CRUN_VERSION}-linux-${CRUN_ARCH}"
    start_spinner "Downloading crun ${CRUN_VERSION}..."
    if curl -fsSL -o "${TMPDIR}/${CRUN_BIN}" "$CRUN_URL"; then
      # Validate downloaded binary
      CRUN_DL_SIZE=$(wc -c < "${TMPDIR}/${CRUN_BIN}" | tr -d ' ')
      if [ "$CRUN_DL_SIZE" -lt 1024 ]; then
        stop_spinner "Downloaded crun binary is too small (${CRUN_DL_SIZE} bytes)" fail
        exit 1
      fi
      if command -v file > /dev/null 2>&1 && ! file "${TMPDIR}/${CRUN_BIN}" | grep -q ELF; then
        stop_spinner "Downloaded crun binary is not a valid ELF executable" fail
        exit 1
      fi
      stop_spinner "Downloaded crun ${CRUN_VERSION}"
      if install -m 755 "${TMPDIR}/${CRUN_BIN}" "${INSTALL_DIR}/${CRUN_BIN}"; then
        step_ok "Installed crun to ${INSTALL_DIR}/${CRUN_BIN}"
      else
        step_fail "Failed to install crun (are you root?)"
        exit 1
      fi
    else
      stop_spinner "Failed to download crun from ${CRUN_URL}" fail
      exit 1
    fi
  fi
fi

# --- Install runsc / gVisor (if bundled) ------------------------------------

RUNSC_BIN="runsc"
if [ -f "${TMPDIR}/${RUNSC_BIN}" ]; then
  start_spinner "Installing runsc to ${INSTALL_DIR}/${RUNSC_BIN}..."
  if install -m 755 "${TMPDIR}/${RUNSC_BIN}" "${INSTALL_DIR}/${RUNSC_BIN}"; then
    stop_spinner "Installed runsc to ${INSTALL_DIR}/${RUNSC_BIN}"
  else
    stop_spinner "Failed to install runsc (are you root?)" fail
    exit 1
  fi
else
  # Fallback: download runsc if not bundled (older release)
  if ! command -v runsc > /dev/null 2>&1; then
    RUNSC_VERSION="${SYFRAH_RUNSC_VERSION:-20260323.0}"
    case "$ARCH" in
      x86_64)  RUNSC_ARCH="x86_64" ;;
      aarch64) RUNSC_ARCH="aarch64" ;;
      *)
        step_fail "Unsupported architecture for runsc: $ARCH"
        exit 1
        ;;
    esac
    RUNSC_URL="https://storage.googleapis.com/gvisor/releases/release/${RUNSC_VERSION}/${RUNSC_ARCH}/runsc"
    start_spinner "Downloading runsc (gVisor) ${RUNSC_VERSION}..."
    if curl -fsSL -o "${TMPDIR}/${RUNSC_BIN}" "$RUNSC_URL"; then
      # Validate downloaded binary
      RUNSC_DL_SIZE=$(wc -c < "${TMPDIR}/${RUNSC_BIN}" | tr -d ' ')
      if [ "$RUNSC_DL_SIZE" -lt 1024 ]; then
        stop_spinner "Downloaded runsc binary is too small (${RUNSC_DL_SIZE} bytes)" fail
        exit 1
      fi
      if command -v file > /dev/null 2>&1 && ! file "${TMPDIR}/${RUNSC_BIN}" | grep -q ELF; then
        stop_spinner "Downloaded runsc binary is not a valid ELF executable" fail
        exit 1
      fi
      stop_spinner "Downloaded runsc ${RUNSC_VERSION}"
      if install -m 755 "${TMPDIR}/${RUNSC_BIN}" "${INSTALL_DIR}/${RUNSC_BIN}"; then
        step_ok "Installed runsc to ${INSTALL_DIR}/${RUNSC_BIN}"
      else
        step_fail "Failed to install runsc (are you root?)"
        exit 1
      fi
    else
      stop_spinner "Failed to download runsc from ${RUNSC_URL}" fail
      exit 1
    fi
  fi
fi

if [ ! -f "${KERNEL_INSTALL_DIR}/${KERNEL_BIN}" ]; then
    # Kernel not in tarball — download from syfrah-images release
    start_spinner "Downloading kernel..."
    KERNEL_URL="https://github.com/sacha-ops/syfrah-images/releases/latest/download/vmlinux.gz"
    KERNEL_SHASUMS_URL="https://github.com/sacha-ops/syfrah-images/releases/latest/download/SHA256SUMS.txt"
    if curl -fsSL -o "${TMPDIR}/vmlinux.gz" "$KERNEL_URL"; then
        stop_spinner "Downloaded kernel"

        # Verify kernel checksum if SHA256SUMS.txt is available
        start_spinner "Verifying kernel checksum..."
        KERNEL_VERIFIED=false
        if curl -fsSL -o "${TMPDIR}/kernel-SHA256SUMS.txt" "$KERNEL_SHASUMS_URL" 2>/dev/null; then
            KERNEL_EXPECTED="$(grep -F "vmlinux.gz" "${TMPDIR}/kernel-SHA256SUMS.txt" | head -1 | awk '{print $1}')"
            if [ -n "$KERNEL_EXPECTED" ]; then
                if command -v sha256sum > /dev/null 2>&1; then
                    KERNEL_ACTUAL="$(sha256sum "${TMPDIR}/vmlinux.gz" | awk '{print $1}')"
                elif command -v shasum > /dev/null 2>&1; then
                    KERNEL_ACTUAL="$(shasum -a 256 "${TMPDIR}/vmlinux.gz" | awk '{print $1}')"
                else
                    KERNEL_ACTUAL=""
                fi
                if [ -n "$KERNEL_ACTUAL" ] && [ "$KERNEL_EXPECTED" = "$KERNEL_ACTUAL" ]; then
                    KERNEL_VERIFIED=true
                    stop_spinner "Kernel checksum verified"
                elif [ -n "$KERNEL_ACTUAL" ]; then
                    stop_spinner "Kernel checksum mismatch (expected ${KERNEL_EXPECTED}, got ${KERNEL_ACTUAL})" fail
                    exit 1
                else
                    stop_spinner "No sha256sum/shasum available — kernel checksum not verified" fail
                fi
            else
                stop_spinner "vmlinux.gz not found in SHA256SUMS.txt — kernel checksum not verified" fail
            fi
        else
            stop_spinner "SHA256SUMS.txt not available — kernel checksum not verified" fail
        fi

        start_spinner "Installing kernel..."
        gunzip -f "${TMPDIR}/vmlinux.gz"
        mkdir -p "$KERNEL_INSTALL_DIR"
        install -m 644 "${TMPDIR}/vmlinux" "${KERNEL_INSTALL_DIR}/vmlinux"
        stop_spinner "Installed kernel to ${KERNEL_INSTALL_DIR}/vmlinux"
    else
        stop_spinner "Could not download kernel (compute layer will download at first use)" fail
    fi
fi

# --- Create data directories ------------------------------------------------

mkdir -p /opt/syfrah/images /opt/syfrah/kernels /opt/syfrah/instances /run/syfrah/vms /usr/local/lib/syfrah

# --- Install prerequisites --------------------------------------------------

# WireGuard (required for fabric mesh)
if ! command -v wg > /dev/null 2>&1; then
  start_spinner "Installing wireguard-tools..."
  if apt-get update -qq > /dev/null 2>&1 && apt-get install -y -qq wireguard-tools > /dev/null 2>&1; then
    stop_spinner "Installed wireguard-tools"
  elif yum install -y wireguard-tools > /dev/null 2>&1; then
    stop_spinner "Installed wireguard-tools"
  else
    stop_spinner "Could not install wireguard-tools — install manually" fail
  fi
fi

# Load WireGuard kernel module
modprobe wireguard 2>/dev/null || true

# nftables (required for security groups, NAT, anti-spoofing)
if ! command -v nft > /dev/null 2>&1; then
  start_spinner "Installing nftables..."
  if apt-get install -y -qq nftables > /dev/null 2>&1; then
    stop_spinner "Installed nftables"
  elif yum install -y nftables > /dev/null 2>&1; then
    stop_spinner "Installed nftables"
  else
    stop_spinner "Could not install nftables — install manually" fail
  fi
fi

# Container runtime check (informational)
if ! command -v crun > /dev/null 2>&1 && ! command -v cloud-hypervisor > /dev/null 2>&1; then
  printf "  %b %s\n" "\342\232\240" "No compute runtime found. Install crun for containers:"
  printf "    apt-get install -y crun\n"
fi

# --- Verify -----------------------------------------------------------------

EXPECTED_VERSION="${VERSION#v}"

if command -v "$BIN" > /dev/null 2>&1; then
  start_spinner "Verifying installation (this may take a moment)..."
  ACTUAL_VERSION=$("$BIN" --version 2>/dev/null | awk '{print $2}') || true
  if [ -z "$ACTUAL_VERSION" ]; then
    stop_spinner "Could not read version from ${BIN} --version" fail
    exit 1
  fi
  if [ "$ACTUAL_VERSION" != "$EXPECTED_VERSION" ]; then
    stop_spinner "Version mismatch: expected ${EXPECTED_VERSION}, got ${ACTUAL_VERSION}" fail
    exit 1
  fi
  stop_spinner "Verified: ${BIN} ${ACTUAL_VERSION}"
else
  step_fail "${BIN} was installed to ${INSTALL_DIR} but is not on PATH"
  printf "    Add %s to your PATH, then run: %s --version\n" "$INSTALL_DIR" "$BIN" >&2
  exit 1
fi

# --- Done -------------------------------------------------------------------

printf "\n%s v%s installed successfully.\n" "$BIN" "$EXPECTED_VERSION"
printf "Run 'syfrah fabric init --name my-mesh' to get started.\n"
