# Release Validation

`bypass-io` uses a conservative release path. The repository validates package
contents before any crate is published.

## Workflow

Release validation is handled by:

```text
.github/workflows/release.yml
```

It runs on:

- manual `workflow_dispatch`
- version tags matching `v*.*.*`

The workflow runs:

```bash
cargo package --workspace
```

That command builds the crates from their packaged contents, not directly from
the working tree. It catches missing files, invalid package metadata, and path
dependency issues before a publish attempt.

## Local Check

Run the same validation locally before creating a tag:

```bash
cargo package --workspace
```

Use `--allow-dirty` only when intentionally checking uncommitted packaging
changes:

```bash
cargo package --workspace --allow-dirty
```

## Publishing Boundary

The release workflow does not publish crates. Publishing should remain a
separate explicit step after package validation, CI, native build checks, and
the intended version review have all passed.
