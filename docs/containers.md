# Container Images

The default project does not require containers. These Dockerfiles are opt-in
tools for reproducible validation environments.

## Lightweight Rust Image

Build:

```bash
docker build -f docker/rust/Dockerfile -t bypass-io:rust .
```

Run the default validation set:

```bash
docker run --rm -it -v "$PWD":/workspace bypass-io:rust \
  bash -lc 'cargo fmt --all -- --check && cargo test --workspace'
```

This image installs Rust, rustfmt, clippy, and normal build tools. It does not
build SPDK or DPDK.

## SPDK Image

Build:

```bash
docker build -f docker/spdk/Dockerfile \
  --build-arg SPDK_REF=master \
  --build-arg SPDK_JOBS=2 \
  -t bypass-io:spdk .
```

Run Rust-side SPDK checks:

```bash
docker run --rm -it -v "$PWD":/workspace bypass-io:spdk \
  bash -lc 'cargo test --features spdk && cargo clippy --all-targets --all-features -- -D warnings'
```

This image is expected to be large. It clones SPDK, initializes submodules,
installs SPDK package dependencies, configures SPDK, and builds it.

Real SPDK NVMe I/O still needs host-level hugepages, device binding, and
permissions. A container image alone is not hardware validation.

## DPDK Image

Build:

```bash
docker build -f docker/dpdk/Dockerfile \
  --build-arg DPDK_REF=main \
  --build-arg DPDK_JOBS=2 \
  -t bypass-io:dpdk .
```

Run Rust-side DPDK checks:

```bash
docker run --rm -it -v "$PWD":/workspace bypass-io:dpdk \
  bash -lc 'cargo test --features dpdk && cargo clippy --all-targets --all-features -- -D warnings'
```

This image is also expected to be large. It builds DPDK with Meson and Ninja.

Real DPDK packet I/O still needs host-level hugepages, NIC binding, and
permissions. A container image alone is not hardware validation.

## Size Guidance

Use `bypass-io:rust` for normal development. Use `bypass-io:spdk` and
`bypass-io:dpdk` only when validating native dependency environments. The native
images can consume multiple gigabytes because they include C source trees,
system packages, and build artifacts.
