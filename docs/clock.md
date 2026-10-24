# clock: explicit legacy and managed entrypoints

`clock-managed` is the production managed API 1.2 entrypoint for identity
`github:1431055733/clock`. it requires only `status`, selects no optional grants
and uses the shared [managed transport](managed-status.md). it does not accept
legacy hello/status as an alternative admission path. the original `clock`
entrypoint retains legacy 1.0 for compatibility and its original regression tests.
both call the same `clock::status` renderer; time formatting is not copied.

## managed output and authority

after valid identity/API/session/generation admission, the application-footer
target returns one real `chrono::Local::now()` value formatted as `HH:MM:SS`,
tooltip `Local time`, priority `5` and `refresh_ms: 1000`. refresh requests host
polling; the process does not emit unsolicited one-second updates.

the application header returns an empty status with the same refresh interval.
pane/workspace targets require authority this profile never selects, so the
transport refuses them before the callback. terminal/cwd/settings data are not
needed or granted. stale bindings and replayed request ids cannot produce a new
managed reply. shutdown or stdin EOF ends the process; invalid managed input
fails without downgrading to legacy behavior. exact framing/refusal details remain
in the managed transport contract.

## timezones: renderer behavior is not host integration

the renderer is unchanged. an explicitly supplied child `TZ` value therefore goes
to the same `chrono::Local` implementation as legacy clock. it is not a new
timezone parser and does not promise libc-equivalent interpretation of every
POSIX expression, file form or malformed/non-UTF-8 value. `TZDIR` behavior and
arbitrary inherited environments are not a managed contract.

the managed clock tests exercise the actual production executable. UTC/Tokyo
values are checked against request-time UTC fences, with observed advancement
after 1100 ms and the exact 1000-ms refresh. separate real legacy processes fence
the managed renderer for absent/empty, POSIX, colon/file and unrecognized/non-UTF-8
inputs without mutating the test runner's environment. this comparison records
legacy behavior, not a claim that every expression names a valid timezone.

**host timezone parity is not established here.** the current host baseline
`28e007bc077b0b26df3b93da1daab0cec736c780` clears `TZ` for managed children.
direct child tests set or remove `TZ` themselves and cannot prove host forwarding.
a separately reviewed bounded environment change and actual PluginHost interaction
on both native architectures are required before accepting task 2.2 parity.

## distribution boundary

there is no static clock manifest, installable catalogue, package or release in
this slice. future task 2.5 must explicitly choose entrypoint `clock-managed` for
plugin id `clock` and reject missing/wrong wiring, not guess or fall back to
`clock`. the retained legacy installer is not a managed installation route.
building either binary grants no installation, access review or execution consent.
the host's old payload remains untouched until verified legacy retention/cutover.
