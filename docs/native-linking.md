# Native Link Checks

`bypass-io` keeps native SPDK and DPDK linkage opt-in. Normal commands such as
`cargo test --workspace --all-features` must stay usable on machines that do
not have SPDK, DPDK, hugepages, or bound devices.

The native link checks below are for dedicated build hosts. They validate that
the Rust crate can receive native link flags and that Cargo will fail early if
the configured native libraries are missing.

They do not yet enable real SPDK NVMe I/O or DPDK packet I/O. The safe runtime
adapters still return `RuntimeUnavailable` until the native runtime
implementation adds and hardware-tests the C call paths.

## SPDK

Enable SPDK native link flags by setting `BYPASS_IO_NATIVE_SPDK=1` while
building with the `spdk` feature:

```bash
BYPASS_IO_NATIVE_SPDK=1 \
SPDK_LIB_DIR=/opt/spdk/build/lib \
cargo test -p bypass-io --features spdk
```

Prefer SPDK's generated pkg-config metadata when it is available:

```bash
BYPASS_IO_NATIVE_SPDK=1 \
SPDK_USE_PKG_CONFIG=1 \
PKG_CONFIG_PATH=/opt/spdk/build/lib/pkgconfig \
cargo test -p bypass-io --features spdk
```

`SPDK_PKG_CONFIG_PATH` can be used instead of `PKG_CONFIG_PATH` when you want
the override to apply only to the `bypass-io` SPDK build script.

By default, the build script links these SPDK libraries as static libraries:

```text
spdk_nvme,spdk_env_dpdk,spdk_util
```

Override the list when your SPDK build needs a different set:

```bash
BYPASS_IO_NATIVE_SPDK=1 \
SPDK_LIB_DIR=/opt/spdk/build/lib \
SPDK_LIBS=spdk_nvme,spdk_env_dpdk,spdk_util \
SPDK_SYSTEM_LIBS=numa,dl,pthread,rt \
cargo test -p bypass-io --features spdk
```

`SPDK_LINK_KIND` defaults to `static`. Set it to `dylib` only when validating an
SPDK build that provides shared libraries with matching runtime loader paths.

## DPDK

Enable DPDK native link flags by setting `BYPASS_IO_NATIVE_DPDK=1` while
building with the `dpdk` feature:

```bash
BYPASS_IO_NATIVE_DPDK=1 \
cargo test -p bypass-io --features dpdk
```

The build script uses `pkg-config --libs libdpdk`. If your DPDK `.pc` file is
not in the default search path, set `PKG_CONFIG_PATH`:

```bash
BYPASS_IO_NATIVE_DPDK=1 \
PKG_CONFIG_PATH=/opt/dpdk/lib/x86_64-linux-gnu/pkgconfig \
cargo test -p bypass-io --features dpdk
```

Use `DPDK_PKG_CONFIG_NAME` if the package name differs from `libdpdk`.

## Runtime Status

When the native link check is not enabled, `SpdkBackend::native_status()` and
`DpdkBackend::native_status()` report `linked = false`.

When the corresponding native link check is enabled and the build succeeds,
the status reports `linked = true`, but the detail string still states that the
native runtime adapter scaffold has I/O disabled. This distinction matters:

- `linked = true`: Cargo accepted native link flags for the build.
- `RuntimeUnavailable`: the Rust backend still does not submit real native I/O.

The native-runtime implementation must replace the validation runtime with
audited SPDK/DPDK call paths before benchmarks or applications rely on real
hardware I/O.

See `docs/native-runtime-adapters.md` for the adapter scaffolds and safety
requirements that must be satisfied before native I/O is enabled.
