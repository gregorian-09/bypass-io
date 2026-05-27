# Hardware Validation

Hardware validation is the Tier 3 path for `bypass-io`. It is intentionally
separate from normal local checks, lightweight CI, and native dependency build
CI because real SPDK/DPDK validation depends on host-level state.

This repository can check whether a host is ready for hardware validation, but
the current Rust backends still report native runtime status as `linked =
false`. That means the hardware workflow verifies the machine and Rust feature
surface today; it does not yet claim real NVMe or NIC I/O through native SPDK or
DPDK symbols.

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

Run it with explicit PCI devices and required hugepages:

```bash
bash tools/hardware/validate_host.sh \
  --spdk-pci 0000:01:00.0 \
  --dpdk-pci 0000:02:00.0 \
  --require-hugepages \
  --check-spdk \
  --check-dpdk
```

The output uses one line per check:

```text
check=hugepages status=ok detail="total=1024 free=1024 size=2048 kB"
```

`status=warn` means the host is missing something useful but the script was not
asked to require it. `status=fail` means a required check failed and the script
exits non-zero.

## GitHub Actions Workflow

Manual workflow:

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
- `test -z "$(git ls-files .internal)"`

## Boundary

This phase validates host readiness and Rust feature gates. True hardware I/O
still requires a later native-runtime phase where `SpdkBackend::native_status()`
or `DpdkBackend::native_status()` reports `linked = true`, native link checks
pass on the target host, native symbols are actually called, and
device-specific I/O tests are executed against bound hardware.

See `docs/native-linking.md` for the opt-in native link-check environment
variables, and `docs/native-runtime-adapters.md` for the unsafe adapter boundary
that must be completed before real hardware I/O is enabled.
