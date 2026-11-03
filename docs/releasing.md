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

An unchanged plugin is not rebuilt or shipped again. Rebuilding it for a new tag would give
the same content but not the same archive (the package records the source commit in
`SOURCE.json`, and the catalogue names the tag's commit as the version's `revision`), and
the application treats the same version with another package digest or another revision
as a publisher inconsistency. Instead, the new catalogue carries the plugin's previous
entry over verbatim, with a `release` reference to the immutable release that holds its
files (see [Carrying unchanged plugins over](#carrying-unchanged-plugins-over)).

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

When it fails because a version rose without a change or fell, revert the version.

### The release pull request

The release pull request raises the versions. It is opened by a command with the operator's
own credentials, so the ordinary required checks run on it (a pull request opened by the
workflow token would not start them). No bot opens pull requests.

1. On a clean `main` that equals `origin/main`, take the build records of that commit:
   the `plugin-version-builds-<arch>-<sha>` artifacts of its `Locked Rust source checks`
   run (`build.json` from `author-packages`, the packaging a release uses). Their
   `revision` must be the checked out commit; the tool refuses any other.
2. Run `among-tools plugin-release-pr . Among-Terminal/among-plugins [--rc N] [--dry-run]
   amd64/build.json arm64/build.json`. It fetches the previous release itself (so its tag
   and its catalogue come from the same release), prints the plan, and without `--dry-run`
   creates the branch `release/<label>`, raises the manifests, commits
   `chore(release): <label>`, pushes and opens the pull request. It refuses when the label
   is already a tag, locally or on the remote. If the commit or the push fails, it puts the
   working tree back on `main` and deletes the branch; if only opening the pull request
   fails, it says that the branch was pushed. `plugin-release-plan` prints the same plan as
   JSON and touches nothing.

How the plan is made:

- A plugin is changed when its packaged content differs from the previous release (the rules
  above); a new plugin is not bumped.
- The level comes from the Conventional Commit titles since the previous tag. For a plugin
  they are the commits scoped with its id and the commits whose scope names no plugin (shared
  code, no scope); a commit scoped to another plugin does not count. `feat` raises the minor
  version, `fix` and `perf` the patch version, and `!` before the colon or a line starting
  with `BREAKING CHANGE:` in the commit body the major version, but while the major version
  is zero a breaking change raises the minor one. The body footer counts only when it is in
  the commit message that lands on `main`; a squash commit takes it from the pull request
  description. A changed plugin without such a commit gets a patch. Other types (`docs`,
  `test`, `ci`, `build`, `refactor`, `chore`) do not count.
- The snapshot label is the previous tag raised by the largest level among the changed plugins.
  The first release is `v0.2.0`. If the previous tag is a candidate, the label stays and only
  `--rc` moves the candidate number. `--rc N` appends `-rc.N`.

Merging the pull request is the owner's decision. The `Snapshot tag` workflow then runs after
the `Locked Rust source checks` of the merge commit have passed, and only for a push to `main`
whose title is `chore(release): ...`. The commit title is not trusted. Before it creates
anything it checks, with `snapshot-verify`:

- the commit changes nothing but the `version` line of plugin manifests;
- the build records are those of this very commit, built by the checks run that just passed;
- the strict version check passes against the previous release as it is now: every changed
  plugin carries a raised version, no version rose without a change;
- the versions are the ones the commits since the previous release call for, and the label in
  the title is the next label computed from them.

Only then it creates the single tag `v<label>` on the commit, with the workflow token. An
existing tag at the same commit is confirmed, a tag anywhere else fails the run, and a tag is
never moved. A tag created this way starts no `push` workflow, and this workflow starts none:
the preparation and the publication stay manual runs of `Official plugin candidate` on the
tag. (The workflow does not dispatch it, because the pinned release workflow of that file
does not carry unchanged plugins over yet; raising that pin to a commit that does is a
separate change.) If another release is published between the checks and the tag job, the
check against "the previous release as it is now" fails and no tag is made; the release pull
request is then rebuilt on the new state.

### Carrying unchanged plugins over

A plugin that did not change is not rebuilt or shipped again; see above for why. In a run
for a release tag the release workflow fetches the previous release with `previous-release`
(a catalogue and a record of the release) and passes it to `author-catalogue --previous`:

- a plugin whose version and packaged content equal the previous release's is left out of
  the assets, and its catalogue entry is the previous entry, verbatim, with
  `"release": {"id": <numeric release id>, "tag": "<tag>"}` naming the immutable release that
  holds its files; an entry that already carries a reference keeps it;
- the command fails when the previous release is a draft or not immutable, when an unchanged
  plugin has no entry in the previous catalogue, when content and version disagree, and when
  nothing changed at all;
- the asset inventory lists only what is uploaded, so `verify-release` needs no change;
  `prepared-inventory` accepts a carried entry without its package, refuses one whose package
  is also shipped, and refuses a release that ships no plugin of its own;
- `verify-references` then checks that every carried entry equals the previous catalogue's
  entry apart from the reference, that the reference is the previous entry's own or the
  previous release, and that every referenced release is published and immutable. The
  publish job runs it again against the previous release as it is at publication time, so
  a prepared candidate whose predecessor changed is refused.

A check run on a pull request (not for a release tag) catalogues every plugin, so that it
passes when nothing changed.

### Release candidates

A candidate is a prerelease of the same repository with a tag `v<label>-rc.N`. Plugin
manifests keep the versions they will have in the stable release; the candidate suffix is
only in the tag.

1. Open the release pull request with `--rc N`, so its title is
   `chore(release): v<label>-rc.N`, and let the owner merge it. The `Snapshot tag` workflow
   creates the tag `v<label>-rc.N` on the merge commit.
2. Run `Official plugin candidate` on that tag with `publish` off. It prepares and inspects
   the bytes (see [author-release.md](author-release.md#publishing-approved-preparation-bytes)).
3. After the owner approves the exact tag, commit and prepared bytes, the owner runs the
   workflow with `publish`, `prerelease` and the approved run, artifact and checksum
   inputs. The release is a prerelease, so the application's latest lookup ignores it.
4. Install it with the application's development mode (`--plugin-development`, "Use
   latest prerelease" for the repository).
5. The stable release is the tag `v<label>` on the same commit, with the same bytes. Find the
   commit with `among-tools resolve-tag <repository-id> v<label>-rc.N`, and the owner creates
   the tag at it (for example `gh api repos/Among-Terminal/among-plugins/git/refs -f
   ref=refs/tags/v<label> -f sha=<commit>`), prepares and approves the stable preparation, and
   publishes it as above. No workflow creates the stable tag. A fix after a candidate changes
   a plugin, and the candidate is then the previous release, so the next release pull request
   raises that plugin's version; make another candidate (`--rc N+1`).

Publishing and the owner's approval are separate steps that no pull request and no workflow
here performs; the only tag a workflow creates is the snapshot tag, after the owner has
merged a release pull request and the commit has been verified again.

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
