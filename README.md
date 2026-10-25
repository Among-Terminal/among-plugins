# among-plugins

the independent source home being prepared for Among's official `clock`,
`system-status` and `pane-pwd` processes.

## extraction baseline, not a managed release

the original `clock`, `system-status` and `pane-pwd` entrypoints preserve their
legacy hello 1.0 behavior and regression tests. `clock-managed` is a separate
production API 1.2 entrypoint using the same clock renderer, not a protocol
fixture or a fallback mode. see [clock contracts](docs/clock.md).
there is no installable managed catalogue or release in this snapshot.

the retained crate name `among-example-plugins` and version `0.1.2` identify the
upstream baseline, not a newly chosen independent plugin release version. future
plugin versions are independent of the Among application version.

[provenance](PROVENANCE.md) records the exact upstream commit, path mapping and
original-source digests. source remains in Among during the transition: no host
build, package payload, legacy installation or user setting is removed here.
host cutover still requires tested legacy retention and rollback paths.

## development

the package builds from this checkout alone; it does not fetch or link the Among
application, GTK or private host modules. use Rust `1.98.1`:

```sh
cargo +1.98.1 fmt --all --check
cargo +1.98.1 clippy --locked --all-targets -- -D warnings
cargo +1.98.1 build --locked --example managed-status
cargo +1.98.1 test --locked --all-targets
cargo +1.98.1 build --release --locked --bins
```

the native amd64/arm64 workflow runs those commands, including both clock
entrypoints. unchanged legacy tests retain real stdio, advancing local/TZ time,
pane-specific directories, resource calculations and initial N/A/target refusal.
new clock tests target the actual `clock-managed` executable, strict admission,
correlation, real time advancement and timezone behavior relative to the legacy
renderer. writing those tests is not evidence that they have passed: current-head
CI and actual host timezone integration remain separate acceptance gates. these
tests do not establish live CPU/MEM/DISK measurements or graphical status output.

`examples/install.rs` is retained legacy code for provenance and regression
coverage, not the future repository installer or an instruction to modify a
personal profile. new managed installation, access review and enablement must
remain explicit; discovery must not start a process.
its unchanged help text still names the upstream `scripts/install-plugins.sh`,
which is absent here; it is not a supported standalone installation entry point.

## licence and remaining work

original code is Apache-2.0; see [LICENSE](LICENSE), [NOTICE](NOTICE) and the
unchanged [upstream notice](provenance/among-NOTICE). dependency licence/source
delivery for future binary packages needs its own complete inventory.

the shared [managed status transport](docs/managed-status.md) supplies strict
admission, grants and correlated bounded stdio. its `managed-status` example is
still only a protocol fixture. `clock-managed` consumes that transport and the
real clock renderer; the three legacy entrypoints remain separate. neither the
fixture's host driver nor client-only clock tests establish production host
timezone parity or authenticated installation.

remaining API 1.2 ports, host timezone parity, static manifests, catalogue/artifact
validation, release automation and real installed lifecycle checks are separate
work. future clock packages must explicitly select `clock-managed`, never infer
the legacy `clock` executable from its plugin id or silently fall back. no tag,
release or package publication is performed by this quality workflow.
