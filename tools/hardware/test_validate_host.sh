#!/usr/bin/env bash
set -eu

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "${script_dir}/../.." && pwd)
validate_script="${repo_root}/tools/hardware/validate_host.sh"

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

assert_contains() {
  local text=$1
  local expected=$2

  if [[ "$text" != *"$expected"* ]]; then
    printf 'expected output to contain: %s\n' "$expected" >&2
    printf 'actual output:\n%s\n' "$text" >&2
    exit 1
  fi
}

make_roots() {
  local root=$1

  mkdir -p \
    "${root}/proc" \
    "${root}/dev/vfio" \
    "${root}/sys/bus/pci/drivers/vfio-pci" \
    "${root}/sys/bus/pci/devices/0000:01:00.0" \
    "${root}/sys/bus/pci/devices/0000:02:00.0"

  touch "${root}/dev/vfio/vfio"
  ln -s ../../drivers/vfio-pci "${root}/sys/bus/pci/devices/0000:01:00.0/driver"
  ln -s ../../drivers/vfio-pci "${root}/sys/bus/pci/devices/0000:02:00.0/driver"
}

run_with_roots() {
  local root=$1
  shift

  BYPASS_IO_PROC_ROOT="${root}/proc" \
    BYPASS_IO_SYS_ROOT="${root}/sys" \
    BYPASS_IO_DEV_ROOT="${root}/dev" \
    bash "$validate_script" "$@"
}

ready_root="${tmpdir}/ready"
make_roots "$ready_root"
cat > "${ready_root}/proc/meminfo" <<'MEMINFO'
HugePages_Total:      16
HugePages_Free:       12
Hugepagesize:       2048 kB
MEMINFO

ready_output=$(run_with_roots "$ready_root" \
  --require-hugepages \
  --spdk-pci 0000:01:00.0 \
  --dpdk-pci 0000:02:00.0)

assert_contains "$ready_output" 'check=hugepages status=ok'
assert_contains "$ready_output" 'check=vfio status=ok'
assert_contains "$ready_output" 'check=vfio_pci status=ok'
assert_contains "$ready_output" 'check=spdk_pci status=ok detail="device=0000:01:00.0 driver=vfio-pci"'
assert_contains "$ready_output" 'check=dpdk_pci status=ok detail="device=0000:02:00.0 driver=vfio-pci"'
assert_contains "$ready_output" 'check=summary status=ok'

no_hugepages_root="${tmpdir}/no-hugepages"
make_roots "$no_hugepages_root"
cat > "${no_hugepages_root}/proc/meminfo" <<'MEMINFO'
HugePages_Total:       0
HugePages_Free:        0
Hugepagesize:       2048 kB
MEMINFO

if no_hugepages_output=$(run_with_roots "$no_hugepages_root" --require-hugepages 2>&1); then
  printf 'expected hugepage-required fixture to fail\n' >&2
  printf 'actual output:\n%s\n' "$no_hugepages_output" >&2
  exit 1
fi
assert_contains "$no_hugepages_output" 'check=hugepages status=fail'
assert_contains "$no_hugepages_output" 'check=summary status=fail'

missing_pci_root="${tmpdir}/missing-pci"
make_roots "$missing_pci_root"
cat > "${missing_pci_root}/proc/meminfo" <<'MEMINFO'
HugePages_Total:       8
HugePages_Free:        8
Hugepagesize:       2048 kB
MEMINFO

if missing_pci_output=$(run_with_roots "$missing_pci_root" --spdk-pci 0000:03:00.0 2>&1); then
  printf 'expected missing PCI fixture to fail\n' >&2
  printf 'actual output:\n%s\n' "$missing_pci_output" >&2
  exit 1
fi
assert_contains "$missing_pci_output" 'check=spdk_pci status=fail'
assert_contains "$missing_pci_output" 'check=summary status=fail'

printf 'validate_host fixture tests passed\n'
