# Independent plugin packages

Plugin versions belong to each static `plugins/<id>/among-plugin.json`, not the
application or the historical Cargo package version. The first preparation uses
`0.1.0-rc.1`; it creates no Git tag or published release. Change an individual
manifest's version when that plugin's release changes. The README is the single
human-readable catalogue; this guide intentionally does not maintain another list.

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

The installer catalogue and plugin manifests remain schema1. Producer schema2
additionally requires the actual native builder's Rust release/commit/host,
compiler hash, installed component manifests and runtime rlib digests. Packaging
refuses absent/changed licence files, missing std/core/alloc/compiler-builtins
inputs or mismatched compiler/native host. Native inspection rereads those inputs
and compares its receipt; inspection of another architecture never executes it.

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

## Separately authorized release and catalogue

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
