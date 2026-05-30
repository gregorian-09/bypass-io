# SPDK CI

SPDK is intentionally handled by an opt-in GitHub Actions workflow instead of a
required local setup. Building SPDK pulls a large native dependency tree, uses
noticeable disk space, and can take long enough that it should not be part of
every local edit-test cycle.

## Manual Workflow

Workflow file:

```text
.github/workflows/spdk-native.yml
```

Run it from GitHub:

1. Open the repository on GitHub.
2. Go to **Actions**.
3. Select **SPDK Native**.
4. Choose **Run workflow**.
5. Pick an SPDK ref and build parallelism.

The default workflow:

- Checks out this repository.
- Checks out `spdk/spdk`.
- Runs SPDK's `scripts/pkgdep.sh`.
- Runs `./configure`.
- Runs `make`.
- Runs `cargo test --features spdk`.
- Runs `cargo test --all-features`.
- Runs an opt-in native SPDK link check using SPDK's generated pkg-config
  metadata.
- Runs `cargo clippy --all-targets --all-features -- -D warnings`.

SPDK's own unit tests are optional because they add more time and log volume.

## Local Commands

Use these only if you explicitly want a local SPDK build:

```bash
git clone https://github.com/spdk/spdk.git
cd spdk
git submodule update --init
sudo ./scripts/pkgdep.sh
./configure
make -j"$(nproc)"
```

To run SPDK against real NVMe hardware, the machine also needs hugepages and a
device bound to VFIO or UIO. That is a machine-level setup step, not a normal
Rust development prerequisite.

For this crate's Rust-side feature checks, local SPDK is not required:

```bash
cargo test --features spdk
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

To validate native SPDK link flags on a host that already built SPDK, use the
opt-in build-script path:

```bash
BYPASS_IO_NATIVE_SPDK=1 \
SPDK_USE_PKG_CONFIG=1 \
PKG_CONFIG_PATH=/path/to/spdk/build/lib/pkgconfig \
cargo test -p bypass-io --features spdk
```

See `docs/native-linking.md` for optional `SPDK_LIBS`, `SPDK_SYSTEM_LIBS`, and
`SPDK_LINK_KIND` overrides.

## Native Runtime Status

`SpdkBackend::native_status()` reports whether the current Rust build enabled
native SPDK link flags. The default repository build reports `linked = false`.
With `BYPASS_IO_NATIVE_SPDK=1`, a successful build reports `linked = true`, but
the safe runtime adapter still returns `RuntimeUnavailable` until native I/O
call paths are implemented and hardware-tested.

The Rust-side backend validates namespace metadata, DMA-buffer eligibility,
target routing, LBA conversion, queue-pair polling delegation, and error
surfaces. It does not claim real NVMe hardware I/O until native SPDK symbols are
linked and tested against bound NVMe devices.
