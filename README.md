# among-plugins

the independent source home being prepared for Among's official `clock`,
`system-status` and `pane-pwd` processes.

## extraction baseline, not a managed release

this first source slice preserves the existing legacy implementations, locked
dependencies, examples and tests without changing their behavior. they still
answer the legacy hello 1.0 protocol; they are **not managed API 1.2 plugins**.
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
cargo +1.98.1 test --locked --all-targets
cargo +1.98.1 build --release --locked --bins
```

the native amd64/arm64 workflow runs those commands. tests retain real process
stdio, advancing local/TZ time, pane-specific directories and resource readings
alongside refusal/fixture controls. these are legacy process/package tests, not
proof of managed host admission or graphical status output.

`examples/install.rs` is retained legacy code for provenance and regression
coverage, not the future repository installer or an instruction to modify a
personal profile. new managed installation, access review and enablement must
remain explicit; discovery must not start a process.

## licence and remaining work

original code is Apache-2.0; see [LICENSE](LICENSE), [NOTICE](NOTICE) and the
unchanged [upstream notice](provenance/among-NOTICE). dependency licence/source
delivery for future binary packages needs its own complete inventory.

managed API 1.2 ports, scoped grants/targets, timezone parity, static manifests,
catalogue/artifact validation, release automation and real host lifecycle checks
are not implemented by this extraction. no tag, release or package publication
is performed by its quality workflow.
