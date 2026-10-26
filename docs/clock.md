# clock: managed entrypoint

`clock-managed` is the production managed API 1.2 entrypoint for identity
`github:1431055733/clock`. it requires only `status`, selects no optional grants
and uses the shared [managed transport](managed-status.md). it does not accept
legacy hello/status as an alternative admission path. there is no separate legacy
executable or protocol loop; the managed callback renders the clock directly.

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

an explicitly supplied child `TZ` value is interpreted by `chrono::Local`. the
plugin does not add a timezone parser or promise libc-equivalent interpretation of every
POSIX expression, file form or malformed/non-UTF-8 value. `TZDIR` behavior and
arbitrary inherited environments are not a managed contract.

the managed clock tests exercise the actual production executable. UTC/Tokyo
values are checked against request-time UTC fences, with observed advancement
after 1100 ms and the exact 1000-ms refresh. separate managed processes exercise
absent/empty, POSIX, colon/file and unrecognized/non-UTF-8 inputs without mutating
the runner's environment. those cases require bounded, valid clock output, not
compatibility with a removed executable or support for every timezone syntax.

direct child tests set or remove `TZ` themselves and cannot prove host forwarding.
that boundary is checked separately through the real PluginHost using exact client
source/binary pins; client-side output tests do not replace it.

## distribution boundary

there is no static clock manifest, installable catalogue, package or release in
this slice. future task 2.5 must explicitly choose entrypoint `clock-managed` for
plugin id `clock` and reject missing/wrong wiring, not guess or fall back to
an executable name from the plugin id. building grants no installation, access
review or execution consent. old-host or legacy-plugin retention is not a release
prerequisite for the ecosystem starting with 0.2.x.
