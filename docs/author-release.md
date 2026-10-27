# Releasing a plugin from your own repository

A plugin repository publishes each version as an immutable GitHub release
whose assets Among 0.2.x installs from the repository's latest release. The
reusable workflow `.github/workflows/plugin-release.yml` of this repository
builds, packages and publishes that release; `among-tools` provides the
commands it runs. Among's installer for this release format is still being
implemented, so a release made this way cannot yet be installed end to end.

## Repository layout

- A Rust package at the repository root with `Cargo.lock`, built with Rust
  1.98.1, `LICENSE` and optionally `NOTICE`.
- `plugins/<id>/among-plugin.json` for each plugin, schema 1, `id` equal to the
  directory name. Among's plugin guide defines the manifest; the tools check
  only `schema_version`, `id`, `version`, `entrypoint` and `api.major` and keep
  the manifest bytes unchanged.
- The executable named by `entrypoint` is the release binary with the same
  file name. Two plugins may not use executables with the same file name.
- A file `icon` is read from `plugins/<id>/<path>` and packed at that path. Any
  other file in a plugin directory refuses the build.

Enable immutable releases in the repository settings before the first tag. The
workflow cannot read that setting with its token; a release published without
it fails the workflow and blocks installation until a new version is
published.

## Calling the workflow

```yaml
on:
  push:
    tags: ['v*']
concurrency:
  group: plugin-release-${{ github.ref }}
  cancel-in-progress: false
jobs:
  release:
    permissions:
      contents: write
    uses: Among-Terminal/among-plugins/.github/workflows/plugin-release.yml@<commit SHA>
```

Reference a full commit SHA. The workflow builds `among-tools` from exactly
that commit (`job.workflow_sha`) and pins the tagged commit before any of your
code runs. Your build job checks the tool's SHA-256 before using it; the final
job, the only one that can write to the repository, downloads the tool again
by artifact ID and digest and runs none of your code. That job takes its token
from the `permissions` of your calling job, so grant exactly `contents: write`
there and nothing more; every other job lowers its token to `contents: read`.
The repository's Actions
settings must allow public reusable workflows. Tags are `v` followed by
letters, digits, `.`, `_` or `-` (at most 128 characters); a tag such as
`v1.0.0+build` is refused.

## Checking pull requests

The same workflow with `publish: false` builds, tests, packages and
catalogues the event commit on both architectures, writes the draft binding
and publishes nothing; it needs only `contents: read`:

```yaml
on:
  pull_request:
permissions:
  contents: read
jobs:
  check:
    uses: Among-Terminal/among-plugins/.github/workflows/plugin-release.yml@<commit SHA>
    with:
      publish: false
```

Call it from `pull_request`, not `pull_request_target`: the event commit is
what gets checked, and for `pull_request_target` that is the base branch, not
the pull request. For `pull_request` it is the merge commit GitHub prepares,
so the check covers the result of merging.

The build job, in both modes, runs `cargo fmt --all --check`,
`cargo clippy --locked --all-targets -- -D warnings`,
`cargo build --locked --all-targets`, `cargo test --locked --all-targets` and
`cargo build --release --locked --bins` with Rust 1.98.1. The debug build of
all targets comes before the tests, so tests that start an example or binary
as a process find it under `target/debug`.

## What a release contains

- `<id>-<version>-linux-amd64.tar.gz` and `…-arm64.tar.gz` for each plugin,
  with the manifest, the executable, the icon if any, `LICENSE`, `NOTICE` if
  present,
  `THIRD_PARTY/**` from the vendored crates, the Rust toolchain notices and
  `SOURCE.json`;
- `source-<commit>.tar.gz`, the source and licence delivery;
- `among-plugins.json`, the schema 2 catalogue of this release;
- `SHA256SUMS` for every other asset.

The catalogue is limited to 1 MiB and a release to 64 assets. Every
`THIRD_PARTY` file is listed in the catalogue, so a release holds a few plugins
rather than the 30 the asset limit would allow.

## Commands

```sh
among-tools author-packages <source-root> <native-binaries> <vendor> <repository-id> <new-output>
among-tools author-catalogue <amd64-build> <arm64-build> <repository-id> <tag> <revision> <new-output>
among-tools release-binding <inventory.json> <workflow-ref> <run-id> <new-output>
among-tools verify-release resume|draft|published <release.json> <inventory.json> <binding> [<latest.json>]
among-tools resolve-tag <repository-id> <tag>
```

`author-catalogue` refuses builds whose repository, revision, plugin set,
manifest bytes or archives differ from the expected values. A draft created by
the workflow carries one `among-release-binding:` line in its body; a later
run resumes only a draft of the same run with that binding and never replaces
assets, moves tags or deletes releases.

## When a run stops

- A re-run of the same workflow run resumes its own draft and uploads only
  the assets still missing, or finishes the checks of a release it already
  published.
- A draft left with an asset that is not fully uploaded, or a draft of
  another run, is never changed: delete that draft by hand and run the
  workflow again.
- A release published while immutable releases were off fails the workflow
  with its URL and is left in place. Among refuses it; enable immutable
  releases and publish a new version.
