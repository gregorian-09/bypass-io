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

For this crate's Rust-side Phase 2 checks, local SPDK is not required:

```bash
cargo test --features spdk
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

## Native Runtime Status

`SpdkBackend::native_status()` reports whether the current Rust build links a
native SPDK adapter. The current repository reports `linked = false`. That is
intentional until a machine with SPDK headers/libraries, configured hugepages,
VFIO/UIO permissions, and a safe completion-ownership design is available for
hardware validation.

The Rust-side backend validates namespace metadata, DMA-buffer eligibility,
target routing, LBA conversion, queue-pair polling delegation, and error
surfaces. It does not claim real NVMe hardware I/O until native SPDK symbols are
linked and tested against bound NVMe devices.
