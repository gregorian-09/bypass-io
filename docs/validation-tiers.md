# Validation Tiers

`bypass-io` uses separate validation tiers so the default project stays light
while native SPDK/DPDK validation remains available when the right environment
exists.

## Tier 0 - Local Rust Checks

Run before committing normal Rust changes:

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps --all-features
```

This tier does not build native SPDK or DPDK.

## Tier 1 - Lightweight GitHub CI

Workflow:

```text
.github/workflows/ci.yml
```

This runs on pull requests and pushes to `main` or `feature/**` branches. It
checks formatting, the workspace test matrix, clippy, rustdoc, a small
`bypass-cli bench db` smoke run, and verifies `.internal` files are not tracked.

## Tier 2 - Native Build CI

Manual workflows:

```text
.github/workflows/spdk-native.yml
.github/workflows/dpdk-native.yml
```

These are intentionally `workflow_dispatch` only. They build large native
dependencies and should not run on every pull request.

Optional container recipes are available under `docker/` for reproducible local
or self-hosted-runner environments:

```text
docker/rust/Dockerfile
docker/spdk/Dockerfile
docker/dpdk/Dockerfile
```

See `docs/containers.md` for build and run commands.

## Tier 3 - Hardware Validation

Hardware validation requires a dedicated machine or self-hosted runner with:

- native SPDK or DPDK libraries
- configured hugepages
- NVMe or NIC devices bound to an appropriate userspace driver
- permissions for the selected driver stack

The current public Rust APIs expose `SpdkBackend::native_status()` and
`DpdkBackend::native_status()` so callers can detect whether a build actually
links native runtime symbols.

## Tier 4 - Performance Regression Tracking

Use `bypass-cli --history` to append JSON-lines benchmark results and compare
against the latest previous run of the same benchmark:

```bash
cargo run --release -p bypass-cli -- bench db \
  --path /tmp/bypass-db \
  --rows-per-batch 10000 \
  --batches 1000 \
  --scan-iterations 20 \
  --compact \
  --history bench-history.jsonl
```
