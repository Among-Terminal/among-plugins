# Contributing

Keep the production plugins independent of the host and its GTK dependencies.
The human catalogue lives only in README.md; add a manifest and link there when
adding a production entry. Managed API declarations must correspond to real
consumers and tests. Do not add implicit executables or a legacy admission path.

Run the locked Rust 1.98.1 format, Clippy, fixture-build and test commands listed
in [releasing](docs/releasing.md) for both the plugin and tooling packages. Native
CI also packages and checks delivered source on amd64 and arm64; a successful
cross-build is not native behavior evidence. Project tooling is Rust in
`tools/among-tools`; shell in CI only orchestrates fixed tools. Do not use Python.

Use a topic branch and PR. Keep incomplete scope or pending required checks in
draft with a specific reason. After current-head CI succeeds, request independent
review in the honest implemented scope. Do not treat compiler success or a
handshake-only check as installed lifecycle or complete release acceptance.

Tags/publication and historical release changes are separate owner decisions;
neither a packaging PR nor a green workflow authorizes them.
