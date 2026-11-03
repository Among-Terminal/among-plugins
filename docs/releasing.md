# Independent plugin packages

Plugin versions belong to each static `plugins/<id>/among-plugin.json`, not the
application or the historical Cargo package version. The current manifests prepare
`0.2.0`; preparation creates no Git tag or release. Change an individual
manifest's version when that plugin's release changes. The README is the single
human-readable catalogue; this guide intentionally does not maintain another list.

For current schema2 release assets, native preparation and publication of exact
approved bytes, use [the reusable workflow](author-release.md#publishing-approved-preparation-bytes).
The official producer commands below also retain the historical schema1 catalogue
format for artifact inspection; that catalogue is not an ordinary 0.2.x install input.

## Independent versions and snapshot releases

Every plugin has its own semantic version in `plugins/<id>/among-plugin.json`. Raise it
only when that plugin changes, and leave it alone when it does not. A plugin changes
when its packaged content changes: the executable, the manifest apart from `version`,
or the attribution and dependency files in the package. Code in the shared `src/`
modules goes into every executable, so a change there changes every plugin that links
it. Compatibility with the application is not the plugin version: it is the `api`
major and minor the manifest declares.

A release is a snapshot of the whole repository:

- one immutable GitHub release with one `v*` tag;
- one `among-plugins.json` (schema 2) among its assets, listing exactly one version of
  every plugin;
- plugins that did not change keep their version, plugins that did carry their raised
  version.

The tag is only a label of the snapshot, not a plugin version. There are no per-plugin
tags such as `clock/v1.2.3`: the application reads the repository's latest release, whose
catalogue describes only that release, and the release workflows accept only tags of the
form `v[A-Za-z0-9._-]`.

Known limitation. Rebuilding an unchanged plugin for a new tag gives the same content but
not the same archive: the package records the source commit in `SOURCE.json`, and the
catalogue names the tag's commit as the version's `revision`. The application treats the
same version with another package digest or another revision as a publisher
inconsistency and offers no update for it. Raising a version only for that reason is
refused by the check below, so this needs a decision about packaging before the second
snapshot is published.

### Checking versions against the previous release

`among-tools changed-plugins` compares the plugins that were just packaged (the
`build.json` that `author-packages` writes, one per architecture) with the catalogue of
the previous release. It must be `author-packages`, because that is what a release
packages with; the output of the official `plugin-packages` has other members and would
differ from every release catalogue. The check fails when:

- a plugin's packaged content changed but its version did not;
- a plugin's version rose but its packaged content did not;
- a plugin's version is lower than before, or is not a semantic version without build
  metadata.

New plugins pass, and a plugin missing from the build is reported and passes. A package
for an architecture that the previous release lacks counts as a change. Three kinds of
member are not content: `SOURCE.json` (names the commit), `THIRD_PARTY/RUST/native/`
(describes the runner's installed toolchain) and `among-plugin.json` (compared as parsed
data without `version`). Across three commits with unchanged plugin code, only these
members differed.

The previous release is the newest published release, a prerelease included, that has an
`among-plugins.json` asset; drafts are skipped. "Newest" is the latest `published_at`
(ties go to the larger release id), not the order GitHub lists them in, and release pages
are read, 100 at a time, until a release with a catalogue turns up. A prerelease counts
because its bytes are already out. When no release has a catalogue, every plugin counts as
new and the check passes. `checks.yml` runs it on every pull request, after packaging, and
further steps prove on the real build that unchanged content passes, that a changed
executable without a version raise fails, and that a version raise over unchanged content
fails with the reason `packaged content is unchanged`.

There are two modes. The default is strict: changed content must come with a raised
version. An ordinary pull request does not raise versions (the release pull request does,
once, for every plugin that changed), so `checks.yml` runs `--pending` for it: changed
content at the previous version passes, while a raise without a change or a lower version
still fails. The run is strict for a release: a branch `release/<label>`, a pull request
titled `chore(release): ...`, or a push to the default branch whose commit title starts
that way. A title is only a hint, so the release commit is verified again before it is
tagged.

```sh
among-tools previous-catalogue Among-Terminal/among-plugins previous-catalogue.json
among-tools changed-plugins --pending previous-catalogue.json dist/author-amd64/build.json dist/author-arm64/build.json
among-tools changed-plugins previous-catalogue.json dist/author-amd64/build.json dist/author-arm64/build.json
```

When a version rose without a content change, or fell, revert the version.

### Release candidates

A candidate is a prerelease of the same repository with a tag `v<label>-rc.N`. Plugin
manifests keep the versions they will have in the stable release; the candidate suffix is
only in the tag.

1. Merge the changes, with the plugin versions raised as the check requires.
2. Run `Official plugin candidate` with `publish` off on the commit to prepare and inspect
   the bytes (see [author-release.md](author-release.md#publishing-approved-preparation-bytes)).
3. After the owner approves the exact tag and commit, the owner creates the tag
   `v<label>-rc.N` and runs the workflow with `publish`, `prerelease` and the approved
   run, artifact and checksum inputs. The release is a prerelease, so the application's
   latest lookup ignores it.
4. Install it with the application's development mode (`--plugin-development`, "Use
   latest prerelease" for the repository).
5. The stable release is the tag `v<label>` on the same commit with the same bytes. A
   fix after a candidate changes a plugin, so the check requires its version to rise
   (the candidate is the previous release); then make another candidate (`-rc.N+1`).

Publishing, tagging and the owner's approval are separate steps that no pull request
performs.

## Native preparation

Use Rust 1.98.1 with its matching `rust-docs`/`rust-src` components, binutils
(`readelf`), glibc `ldd`, Git and Cargo on the actual
amd64/arm64 architecture. No host checkout, GTK, emulator or cross-built binary is
used. Run quality gates in a clean container or the native CI runners:

```sh
rustup component add --toolchain 1.98.1 rust-docs rust-src
cargo +1.98.1 fmt --all --check
cargo +1.98.1 clippy --locked --all-targets -- -D warnings
cargo +1.98.1 build --locked --example managed-status --example managed-pages
cargo +1.98.1 test --locked --all-targets
cargo +1.98.1 build --locked --release --bins
cargo +1.98.1 fmt --manifest-path tools/among-tools/Cargo.toml --all --check
cargo +1.98.1 clippy --manifest-path tools/among-tools/Cargo.toml --locked --all-targets -- -D warnings
cargo +1.98.1 test --manifest-path tools/among-tools/Cargo.toml --locked --all-targets
cargo +1.98.1 build --manifest-path tools/among-tools/Cargo.toml --locked
mkdir dist
cargo +1.98.1 vendor --locked --versioned-dirs --sync tools/among-tools/Cargo.toml vendor > dist/vendor-config.toml
tools/among-tools/target/debug/among-tools plugin-packages "$PWD" "$PWD/target/release" "$PWD/vendor" "$PWD/dist/native"
```

The tool refuses dirty source and existing output directories. Each independent
tar.gz contains its exact shared manifest, one declared executable, original
attribution, dependency licensing texts/metadata and a source receipt. It emits
regular USTAR members only, fixed modes/owners/timestamps, a gzip trailer and
complete member sizes/digests. No installer, hook, runtime build or auto-enable
operation is packaged. Source delivery contains tracked project files plus the
complete versioned Cargo vendor tree and offline source-replacement configuration.
Source collection admits each file against running 32,768-file/256-MiB limits
before reading/retaining it, and bounds directory traversal to 65,536 entries.
Dependency text collection includes README/notice/copyright/license files; this
is material for legal review, not a claim that metadata alone proves compliance.

`build.json` producer schema2 records the exact source, platform, archive digest/member inventory,
GLIBC symbol-version floor measured by `readelf`, and actual DT_NEEDED libraries.
Unmodeled libraries, RPATH/RUNPATH, wrong ELF machine/interpreter, absent symbol
floors and missing native libraries refuse packaging. The baseline compiler
target is used, not `target-cpu=native`. This checks ABI/runtime consistency, not
every CPU extension, minimum kernel, distribution certification or reproducibility.
glibc, libgcc and timezone data are supplied by the target system. No external
command grants or notification authority are declared by these status plugins.

The historical official catalogue and plugin manifests use schema1. Producer schema2
additionally requires the actual native builder's Rust release/commit/host,
compiler hash, installed component manifests and runtime rlib digests. Packaging
refuses absent/changed licence files, missing std/core/alloc/compiler-builtins
inputs or mismatched compiler/native host. Native inspection rereads those inputs
and compares its receipt; inspection of another architecture never executes it.
Empty, whitespace-only or non-UTF-8 attribution/component texts refuse both
collection and package verification, including a self-consistent empty hash.

`THIRD_PARTY/RUST` retains the actual standard-library copyright report, Rust
MIT/Apache texts, rust-src licence files, compiler-builtins/libm/libunwind terms
and source commit. The Fuchsia mutex source notice is retained conservatively
even though Linux GNU does not select that target. The runtime report and source
notices are separate from the application Cargo graph and are not replaced by
unicode-ident's notices. Common notices enter the shared source archive too;
machine-specific component/compiler receipts remain in each native package.
The two source archives must still agree. This is a conservative attribution
superset, not a per-function linker proof, compiler redistribution, independent
toolchain authentication or a claim to redistribute target OS libraries.

Package inspection reopens the archives, checks complete members/hashes/modes and
manifest agreement, then runs each extracted executable through real managed
admission/shutdown on its native architecture. The retained `checks-*` files are
inspection evidence, not extra package members. Full status/lifecycle/host UI
acceptance remains separate; handshake success does not establish installation.

CI retains native outputs under source-SHA-labelled artifacts and also builds
the delivered source offline with an empty Cargo home. CI artifacts are temporary
preparation inputs, not installable GitHub Release assets. Do not advertise an
installation catalogue until real published asset identities are bound.
Failed jobs also retain any partial package/inspection files; those artifacts are
diagnostics, never accepted build inputs merely because they were uploaded.

## Historical schema1 catalogue inspection

This records the original publication procedure. It does not authorize changing
historical releases or publishing a new schema1 installation catalogue. New
schema2 publication follows the exact-byte preparation procedure linked above.

1. Obtain the distinct plugin tag/publication approval. Record the exact tag and
   reviewed source SHA; require current native CI and independent review. Preparing
   the application RC is not permission for a stable plugin/application release.
2. Download both exact-source native artifacts. Verify their checksums, independent
   manifests and source archives. The source archives must have identical digests.
   Preserve their native `build.json` records in separate amd64/arm64 directories.
3. After approval, create the exact tag at that source and a new GitHub Release.
   Upload the six independent package archives and one complete source archive.
   Do not replace historical release assets. Draft and published metadata have
   different availability; a locally generated draft catalogue is not publicly
   installable. Keep ordinary users' install/grants/enable actions explicit.
4. At the same clean source SHA, with `gh` authenticated if inspecting a draft:

   ```sh
   tools/among-tools/target/debug/among-tools plugin-catalogue "$PWD" /path/to/amd64 /path/to/arm64 RELEASE_ID "$PWD/dist/catalogue"
   ```

   `RELEASE_ID` is an actual numeric GitHub release ID, not a version or `latest`.
   The tool queries the pinned numeric repository/publisher, reads its real
   `tag_name`, resolves the exact tag reference (including bounded annotations),
   and requires the resulting commit to match both build records. Required assets
   must have unique names/IDs and matching actual GitHub size/SHA256/upload state.
   It repeats release and complete tag-chain inspection before writing output.
   Recorded draft/prerelease state must remain unchanged; the receipt uses the
   confirmed state. Output above the host's 1-MiB catalogue limit is refused before
   creating the output directory.
   No release/asset IDs, missing platforms or empty success are invented. Observed
   consistency is not independent publisher authentication or protection from tag
   movement after inspection.
5. Publish the approved candidate, re-inspect its public metadata and submit the
   generated `among-plugins.json` at repository root through a reviewed PR. That
   catalogue commit is intentionally distinct from its release-source revision.
   Preserve `release-binding.json` and exact public asset evidence. Obtain current
   host-parser, install/review/enable/output/update/disable/uninstall acceptance
   on both architectures before closing the full distribution task.

The catalogue generator is read-only toward GitHub. It cannot create, upload,
publish, mutate tags, install plugins, grant permissions or enable processes.
