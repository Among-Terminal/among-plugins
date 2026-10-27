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
jobs:
  release:
    permissions:
      contents: write
    uses: Among-Terminal/among-plugins/.github/workflows/plugin-release.yml@<commit SHA>
```

Reference a full commit SHA. The workflow builds `among-tools` from exactly
that commit (`job.workflow_sha`), pins the tagged commit before any of your
code runs, and only its final job can write to the repository.

## What a release contains

- `<id>-<version>-linux-amd64.tar.gz` and `…-arm64.tar.gz` for each plugin,
  with the manifest, the executable, the icon if any, `LICENSE`, `NOTICE`,
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
