# Hardware Validation

Hardware validation is the Tier 3 path for `bypass-io`. It is intentionally
separate from normal local checks, lightweight CI, and native dependency build
CI because real SPDK/DPDK validation depends on host-level state.

This repository can check whether a host is ready for hardware validation. The
native SPDK and DPDK adapters also include runtime-gated C call paths, but they
remain disabled unless the operator explicitly enables them with hardware opt-in
environment variables.

## Host Requirements

A real hardware-validation host should have:

- Linux with access to `/proc` and `/sys`.
- Hugepages configured.
- VFIO or another appropriate userspace driver stack available.
- An NVMe device for SPDK validation, or a NIC for DPDK validation.
- The selected PCI device bound to the expected userspace driver.
- A Rust toolchain with `rustfmt` and `clippy`.

The readiness check is read-only. It does not allocate hugepages, bind devices,
load modules, or change permissions.

## Local Readiness Check

Run the non-mutating host check without required hardware:

```bash
bash tools/hardware/validate_host.sh
```

The CLI exposes the same lightweight readiness view for day-to-day inspection:

```bash
cargo run -p bypass-cli -- doctor native
```

Run it with explicit PCI devices and required hugepages:

```bash
bash tools/hardware/validate_host.sh \
  --spdk-pci 0000:01:00.0 \
  --dpdk-pci 0000:02:00.0 \
  --require-hugepages \
  --check-spdk \
  --check-dpdk
```

Or through the CLI:

```bash
cargo run -p bypass-cli -- doctor native \
  --spdk-pci 0000:01:00.0 \
  --dpdk-pci 0000:02:00.0 \
  --require-hugepages
```

The output uses one line per check:

```text
check=hugepages status=ok detail="total=1024 free=1024 size=2048 kB"
```

`status=warn` means the host is missing something useful but the script was not
asked to require it. `status=fail` means a required check failed and the script
exits non-zero.

## GitHub Actions Workflow

There are two CI paths:

- Hosted CI validates the readiness script with deterministic fixtures. This
  catches regressions in hugepage, VFIO, and PCI-device detection logic without
  needing real hardware.
- The manual hardware workflow validates a real host through a self-hosted
  runner.

Hosted fixture test:

```text
tools/hardware/test_validate_host.sh
```

The fixture test is run by the normal Rust CI workflow on GitHub-hosted Linux.
It simulates readable `/proc`, `/sys`, and `/dev` roots and verifies both
passing and failing readiness cases.

Manual workflow for a real hardware host:

```text
.github/workflows/hardware-validation.yml
```

The workflow is `workflow_dispatch` only and runs on:

```text
[self-hosted, linux, bypass-hardware]
```

That label set prevents this job from running on ordinary GitHub-hosted
runners. A repository owner must attach a dedicated Linux self-hosted runner
with the `bypass-hardware` label.

The workflow runs:

- `cargo fmt --all -- --check`
- `cargo test --workspace --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `tools/hardware/validate_host.sh` with the dispatch inputs

Native hardware I/O is intentionally not enabled by this workflow by default.
To run the hardware-gated paths manually on the runner, set:

```bash
export BYPASS_IO_ENABLE_SPDK_HARDWARE=1
export BYPASS_IO_ENABLE_DPDK_HARDWARE=1
```

## Validation Procedure

Use this sequence to validate the hardware path:

1. Prepare a dedicated Linux host with hugepages and the target PCI devices.
2. Install and start the GitHub self-hosted runner on that host.
3. Add the runner labels `linux` and `bypass-hardware`.
4. Run the local readiness script directly on the host:

```bash
bash tools/hardware/validate_host.sh \
  --spdk-pci 0000:01:00.0 \
  --dpdk-pci 0000:02:00.0 \
  --require-hugepages \
  --check-spdk \
  --check-dpdk
```

5. Trigger the `Hardware Validation` workflow from GitHub Actions with the same
   PCI BDF values.

For the current repository state, a passing hardware workflow means the host is
ready, the Rust feature surface builds, and the selected PCI devices are visible
with the expected driver setup. Real native I/O requires the runtime opt-in
variables above plus native link flags.

## Boundary

This workflow validates host readiness and Rust feature gates. True hardware I/O
requires `SpdkBackend::native_status()` or `DpdkBackend::native_status()` to
report `linked = true`, native link checks to pass on the target host, runtime
hardware opt-in variables to be set, and device-specific I/O tests to execute
against bound hardware.

See `docs/native-linking.md` for the opt-in native link-check environment
variables, and `docs/native-runtime-adapters.md` for the unsafe adapter boundary
that must be completed before real hardware I/O is enabled.
