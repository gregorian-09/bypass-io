# DPDK CI

DPDK is handled by an opt-in GitHub Actions workflow rather than a required
local setup. Building DPDK requires native build tools, a large C source tree,
and enough disk and memory that it should not run during every local edit-test
cycle.

## Manual Workflow

Workflow file:

```text
.github/workflows/dpdk-native.yml
```

Run it from GitHub:

1. Open the repository on GitHub.
2. Go to **Actions**.
3. Select **DPDK Native**.
4. Choose **Run workflow**.
5. Pick a DPDK ref and build parallelism.

The default workflow:

- Checks out this repository.
- Checks out `DPDK/dpdk`.
- Installs Meson, Ninja, compiler, NUMA, pkg-config, and Python ELF tooling.
- Runs `meson setup build`.
- Runs `ninja -C build`.
- Runs `cargo test --features dpdk`.
- Runs `cargo test --all-features`.
- Runs `cargo clippy --all-targets --all-features -- -D warnings`.

DPDK's fast tests are optional because they add time and may depend on runner
capabilities.

## Local Commands

Use these only if you explicitly want a local DPDK build:

```bash
git clone https://github.com/DPDK/dpdk.git
cd dpdk
sudo apt-get update
sudo apt-get install -y build-essential libnuma-dev meson ninja-build pkg-config python3-pyelftools
meson setup build
ninja -C build -j"$(nproc)"
```

To run DPDK against a real NIC, the machine also needs hugepages and a NIC bound
to VFIO or UIO. That is a host-level networking setup step, not a normal Rust
development prerequisite.

For this crate's Rust-side Phase 3 checks, local DPDK is not required:

```bash
cargo test --features dpdk
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

To validate native DPDK link flags on a host that already provides a DPDK
`pkg-config` file, use the opt-in build-script path:

```bash
BYPASS_IO_NATIVE_DPDK=1 \
cargo test -p bypass-io --features dpdk
```

If the package metadata is not in the default search path, set
`PKG_CONFIG_PATH`. See `docs/native-linking.md` for details.

## Native Runtime Status

`DpdkBackend::native_status()` reports whether the current Rust build enabled
native DPDK link flags. The default repository build reports `linked = false`.
With `BYPASS_IO_NATIVE_DPDK=1`, a successful build reports `linked = true`, but
the safe runtime adapter still returns `RuntimeUnavailable` until native I/O
call paths are implemented and hardware-tested.

The Rust-side backend validates EAL/port configuration, queue bounds, target
routing, packet parsing, multicast flow-rule inputs, polling delegation, and
error surfaces. It does not claim real NIC RX/TX until native DPDK symbols are
linked and tested against bound NIC devices.
