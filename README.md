# among-plugins

official plugins for [Among Terminal](https://github.com/Among-Terminal/among),
built as standalone Rust processes without linking the application or GTK.

[catalogue](#catalogue) · [getting started](#getting-started) · [development](#development) · [licence](#licence-and-provenance)

## catalogue

| plugin | purpose | entrypoints | documentation |
| --- | --- | --- | --- |
| clock | local time in the application footer (`HH:MM:SS`) | `clock-managed` — managed API 1.2 | [contract and timezone limits](docs/clock.md) |
| system-status | Linux CPU, memory and root-filesystem usage in the application header; unavailable measurements show `N/A` | `system-status-managed` — managed API 1.2 | [resource contract and namespace limits](docs/system-status.md) |
| pane-pwd | working directory supplied by Among for each pane, displayed in its footer | `pane-pwd-managed` — managed API 1.2 scoped pane adapter | [target, required access and SDK paging](docs/pane-pwd.md) |

each plugin has one catalogue entry covering its purpose, available entrypoints
and documentation. add new plugins here rather than maintaining separate lists.
the ecosystem starts with Among 0.2.x; there is no legacy executable, protocol
fallback or standalone legacy installer. see the [managed transport contract](docs/managed-status.md)
for admission, permissions and stdio framing.

## getting started

**no official plugin release or binary download has been published yet.**
the [releases page](https://github.com/Among-Terminal/among-plugins/releases)
is the place to check for published packages. this checkout has no installable
managed catalogue or supported standalone installer; building a binary does not
install or enable it in Among.

for the application's current plugin setup and API, read
[Among's plugin guide](https://github.com/Among-Terminal/among/blob/main/docs/plugins.md).

## development

clone this repository and use Rust `1.98.1`, as pinned in
[CI](.github/workflows/checks.yml):

```sh
git clone https://github.com/Among-Terminal/among-plugins.git
cd among-plugins
cargo +1.98.1 fmt --all --check
cargo +1.98.1 clippy --locked --all-targets -- -D warnings
cargo +1.98.1 build --locked --example managed-status --example managed-pages
cargo +1.98.1 test --locked --all-targets
cargo +1.98.1 build --release --locked --bins
```

the examples are protocol test fixtures, not catalogue plugins; their builds are
required before the process tests. CI runs these checks
on native Linux amd64 and arm64; release binaries are written to `target/release/`.
the tests cover plugin-side behavior, not installed lifecycle or graphical host
integration. no Among checkout or private SDK is needed to build this package.

the retained crate name `among-example-plugins` and version `0.1.2` identify the
extraction baseline, not a published standalone release. plugin release versions
are independent of the Among application version.

## licence and provenance

original project code is [Apache-2.0](LICENSE). see [NOTICE](NOTICE) for attribution
and dependency redistribution boundaries. [PROVENANCE.md](PROVENANCE.md) records
the extraction from Among, original-source digests and the upstream authorship
trail; the original [upstream notice](provenance/among-NOTICE) is preserved.
