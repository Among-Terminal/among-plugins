# original source provenance

upstream: [Among-Terminal/among](https://github.com/Among-Terminal/among).
the byte-preserved extraction input is commit
[`a23600ff7e6b472bf2801ed89e486e5261b00b0d`](https://github.com/Among-Terminal/among/commit/a23600ff7e6b472bf2801ed89e486e5261b00b0d).

| upstream path at that commit | extracted path |
|---|---|
| `examples/plugins/Cargo.toml`, `Cargo.lock` | `Cargo.toml`, `Cargo.lock` |
| `examples/plugins/src/` | `src/` |
| `examples/plugins/tests/` | `tests/` |
| `examples/plugins/examples/` | `examples/` |
| `LICENSE` | `LICENSE` |
| `NOTICE` | `provenance/among-NOTICE` |

all 17 package files and both original attribution files retain their source
bytes; no test assertion, dependency version or legacy protocol is rewritten in
this import. [upstream history](https://github.com/Among-Terminal/among/commits/a23600ff7e6b472bf2801ed89e486e5261b00b0d/examples/plugins)
preserves the original authorship trail. this repository starts with its own
README bootstrap; the import does not claim those upstream commits as new work.

`provenance/UPSTREAM.sha256` lists those 19 original files using the extracted
paths. it binds the extraction snapshot only: later managed-protocol work will
change source files normally and must retain this historical manifest and Git
history rather than rewrite the original digests to imply an unchanged import.
these hashes establish source correspondence, not publisher authentication or
managed package/release identity.

the new root NOTICE explains this source snapshot's scope; the upstream NOTICE
is kept separately because its application-wide attribution paths are not the
standalone plugin distribution's completed inventory. no compiled dependencies
or user-facing binary package is distributed by this source-only import.

the host still retains its original implementations and payloads until a reviewed,
observed upgrade-retention gate permits cutover. this temporary staging copy is
not a second supported production distribution or an automatic migration.

## later managed transport work

`src/managed/wire.rs` adapts the duplicate-key visitor and string-aware depth
scan from Apache-2.0 Among's `src/plugin/data.rs` and `src/plugin/transport.rs`
at public integration commit
[`5d0d00e6cc5aa0529969b9d0bdbc2cc00b15361c`](https://github.com/Among-Terminal/among/commit/5d0d00e6cc5aa0529969b9d0bdbc2cc00b15361c).
the wire contract follows that commit's managed runtime and plugin guide. this
adaptation is not part of the unchanged initial 19-file extraction manifest;
the original licence and notice remain applicable. it adds no host-source build
dependency or authority to publish a managed release.
