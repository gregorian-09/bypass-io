#!/usr/bin/env bash
set -u

usage() {
  cat <<'USAGE'
Validate a Linux host for bypass-io hardware validation.

This script is intentionally non-mutating. It reports host readiness for
SPDK/DPDK-style validation, but it does not allocate hugepages, bind PCI
devices, load kernel modules, or require root by default.

Usage:
  bash tools/hardware/validate_host.sh [options]

Options:
  --spdk-pci BDF          PCI BDF expected to be used for SPDK/NVMe testing.
  --dpdk-pci BDF          PCI BDF expected to be used for DPDK/NIC testing.
  --require-hugepages     Fail when HugePages_Total is zero.
  --check-spdk            Run cargo test --features spdk.
  --check-dpdk            Run cargo test --features dpdk.
  --help                  Show this help text.

Examples:
  bash tools/hardware/validate_host.sh
  bash tools/hardware/validate_host.sh \
    --spdk-pci 0000:01:00.0 \
    --dpdk-pci 0000:02:00.0 \
    --require-hugepages \
    --check-spdk \
    --check-dpdk
USAGE
}

spdk_pci=""
dpdk_pci=""
require_hugepages=0
check_spdk=0
check_dpdk=0
failures=0

emit() {
  local check=$1
  local status=$2
  local detail=$3

  printf 'check=%s status=%s detail="%s"\n' "$check" "$status" "$detail"
}

pass() {
  emit "$1" "ok" "$2"
}

warn() {
  emit "$1" "warn" "$2"
}

fail() {
  emit "$1" "fail" "$2"
  failures=$((failures + 1))
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --spdk-pci)
      if [[ $# -lt 2 ]]; then
        fail "args" "--spdk-pci requires a PCI BDF"
        break
      fi
      spdk_pci=$2
      shift 2
      ;;
    --dpdk-pci)
      if [[ $# -lt 2 ]]; then
        fail "args" "--dpdk-pci requires a PCI BDF"
        break
      fi
      dpdk_pci=$2
      shift 2
      ;;
    --require-hugepages)
      require_hugepages=1
      shift
      ;;
    --check-spdk)
      check_spdk=1
      shift
      ;;
    --check-dpdk)
      check_dpdk=1
      shift
      ;;
    --help)
      usage
      exit 0
      ;;
    *)
      fail "args" "unknown option: $1"
      shift
      ;;
  esac
done

pass "kernel" "$(uname -srmo)"

if [[ -r /proc/meminfo ]]; then
  huge_total=$(awk '/^HugePages_Total:/ {print $2}' /proc/meminfo)
  huge_free=$(awk '/^HugePages_Free:/ {print $2}' /proc/meminfo)
  huge_size=$(awk '/^Hugepagesize:/ {print $2 " " $3}' /proc/meminfo)

  huge_total=${huge_total:-0}
  huge_free=${huge_free:-0}
  huge_size=${huge_size:-unknown}

  if (( huge_total > 0 )); then
    pass "hugepages" "total=${huge_total} free=${huge_free} size=${huge_size}"
  elif (( require_hugepages )); then
    fail "hugepages" "HugePages_Total=0; configure hugepages before hardware validation"
  else
    warn "hugepages" "HugePages_Total=0; Rust checks can run, but hardware validation needs hugepages"
  fi
else
  if (( require_hugepages )); then
    fail "hugepages" "/proc/meminfo is not readable"
  else
    warn "hugepages" "/proc/meminfo is not readable"
  fi
fi

if [[ -e /dev/vfio/vfio ]]; then
  pass "vfio" "/dev/vfio/vfio is present"
else
  warn "vfio" "/dev/vfio/vfio is not present; VFIO-bound device tests will not run"
fi

if [[ -d /sys/bus/pci/drivers/vfio-pci ]]; then
  pass "vfio_pci" "vfio-pci driver is visible in sysfs"
else
  warn "vfio_pci" "vfio-pci driver is not visible in sysfs"
fi

check_pci_device() {
  local check=$1
  local bdf=$2

  if [[ -z "$bdf" ]]; then
    warn "$check" "no PCI BDF provided"
    return
  fi

  local path="/sys/bus/pci/devices/${bdf}"
  if [[ ! -e "$path" ]]; then
    fail "$check" "PCI device ${bdf} was not found under /sys/bus/pci/devices"
    return
  fi

  if [[ -L "${path}/driver" ]]; then
    local driver
    driver=$(basename "$(readlink "${path}/driver")")
    pass "$check" "device=${bdf} driver=${driver}"
  else
    warn "$check" "device=${bdf} exists but has no bound driver"
  fi
}

check_pci_device "spdk_pci" "$spdk_pci"
check_pci_device "dpdk_pci" "$dpdk_pci"

if (( check_spdk )); then
  if cargo test --features spdk; then
    pass "cargo_spdk" "cargo test --features spdk passed"
  else
    fail "cargo_spdk" "cargo test --features spdk failed"
  fi
fi

if (( check_dpdk )); then
  if cargo test --features dpdk; then
    pass "cargo_dpdk" "cargo test --features dpdk passed"
  else
    fail "cargo_dpdk" "cargo test --features dpdk failed"
  fi
fi

if (( failures > 0 )); then
  emit "summary" "fail" "hardware host validation found ${failures} required failure(s)"
  exit 1
fi

emit "summary" "ok" "hardware host validation completed without required failures"
