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

at extraction, the host retained its original implementations and payloads as
temporary staging. that history is not a promise to support old executables or
an upgrade-retention gate: the managed ecosystem starts with Among 0.2.x.
the original source manifest and Git history remain preserved even when obsolete
legacy code and tests are removed from the current tree.

## later managed transport work

`src/managed/wire.rs` adapts the duplicate-key visitor and string-aware depth
scan from Apache-2.0 Among's `src/plugin/data.rs` and `src/plugin/transport.rs`
at public integration commit
[`5d0d00e6cc5aa0529969b9d0bdbc2cc00b15361c`](https://github.com/Among-Terminal/among/commit/5d0d00e6cc5aa0529969b9d0bdbc2cc00b15361c).
the wire contract follows that commit's managed runtime and plugin guide. this
adaptation is not part of the unchanged initial 19-file extraction manifest;
the original licence and notice remain applicable. it adds no host-source build
dependency or authority to publish a managed release.

## prominent modification notices

The original hashes above are historical and are deliberately unchanged. The
retained derived files now carry file-local notices, independently of Git history:

| file | changes since the extraction input |
| --- | --- |
| `Cargo.toml` | managed entrypoints/SDK dependencies; legacy bins and installer removed |
| `src/lib.rs` | managed SDK exports replace the standalone installer |
| `src/clock.rs` | typed managed target; legacy renderer entry removed |
| `src/pwd.rs` | scoped string-reference lookup; numeric legacy adapter removed |
| `src/protocol.rs` | shared status data retained; permissive legacy loop removed |
| `src/system.rs` | managed target adapter around the existing sampler |
| `src/managed/wire.rs` | standalone bounded adaptation of the named host validators |

Removed legacy source/examples/tests are withdrawals, not unchanged files or
passing old behavior. `Cargo.lock`, `tests/system.rs`, the project licence and
original upstream notice remain byte-preserved where the historical hashes apply.

## linked Rust runtime attribution

Native package production collects the actual pinned builder's Rust1.98.1
standard-library copyright report, licence texts, relevant rust-src notices and
compiler-builtins/libm/libunwind terms. Per-platform receipts retain compiler,
component manifest and runtime rlib digests; the shared source delivery retains
the common runtime notices and exact Rust source commit. These inputs are separate
from the application's Cargo lock graph. See `docs/releasing.md` for the boundary:
this is not compiler redistribution, independent toolchain authentication or a
claim to know every linked function's provenance.
