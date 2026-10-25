# managed status transport (API 1.2)

`managed::run` is the shared plugin-side stdio loop; `managed::serve` exposes the
same loop to bounded I/O tests. neither depends on Among source, GTK, a private
SDK or the application version. the host contract used here is documented in
[Among's API 1.2 section](https://github.com/Among-Terminal/among/blob/5d0d00e6cc5aa0529969b9d0bdbc2cc00b15361c/docs/plugins.md#managed-admission-context-and-settings-api-12)
at that integration head. this is not a new API 2.0.

## admission and authority

a caller supplies its exact manifest identity and required/optional features in
`Profile`. this status-only implementation accepts declarations of `status`,
`context.terminals` and `context.cwd`. status must be required; cwd requires
terminal context. duplicates, overlapping required/optional declarations and
unsupported declarations refuse before reading or emitting data. do not declare
settings, events or notification authority: they have no consumer here.

the first request must be `managed_hello` with major 1, minor 2, matching identity
and nonempty bounded session/generation strings. all required features must be
offered. the response selects only declared offered features, preserving the
terminal/cwd dependency; unrelated offers such as settings are not accepted.
the host owns the five-second startup deadline and grants/review before spawn.
the client replies immediately after validation and emits no earlier status.
repeated hello refuses; no frame can rebind a running process to another session.

## status and lifetime

`status` includes session, generation, numeric u64 request ID, explicit scope and
context. old session/generation and duplicate or decreasing IDs are ignored
before invoking the consumer; current requests retain their exact ID and accepted
binding in stdout. the host's IDs increase per process and never wrap. the client
dispatch is synchronous; page reducers retain only bounded per-request state under
the admitted binding, never delayed workers or saved context for another request.
the host must still reject late replies or closed targets; client validation is
not a substitute for host admission, live resolution or deadlines.

`Scope` exposes typed application/workspace/pane targets. workspace and pane
references deserialize only from canonical u64 decimal **strings**, never JSON
numbers, leading-zero aliases or display indices. application targets permit
app_header/app_footer; workspace targets permit workspace_header/summary; pane
targets permit pane_header/footer. non-application scopes require terminal access.

context must match the admitted session. without terminal access only the session
is allowed. pages have at most 32 terminals and 28 KiB of encoded terminal data.
terminal references must be canonical, unique and within the requested target.
active_workspace and focused_pane are application-only metadata; their presence
in pane/workspace context refuses, including null rather than just noncanonical
or sibling references.
cwd is absent without its grant; an authorized unavailable cwd remains null, not
a fabricated path. paths exceeding 4096 UTF-8 or 8192 encoded bytes refuse rather
than truncate. sibling data and undeclared terminal fields refuse before callback.

the existing `serve`/`run` callback sees the initial context page and returns a
real `protocol::Status`. `run_paged` uses the same engine with a `PageConsumer`:
`Complete` replies or `Continue` retains a bounded copy-only reducer state and
requests the exact returned cursor/scope. status IDs and enumeration IDs are
independent. other statuses continue dispatching while one page is outstanding.
see [paging, local deadlines and retained-state bounds](managed-paging.md).
no configure/configured or events capability is advertised here.

`shutdown` stops with no further output (including before admission); EOF at a
frame boundary exits successfully. partial EOF, malformed/unknown requests or
invalid authority exit unsuccessfully. pipe errors propagate as failure. no
automatic restart, retry, installation or access persistence exists in the client.

## framing

both directions allow 64 KiB **including the newline**, with at most 32 JSON
container levels. input is bounded before parsing, including absent newline;
invalid UTF-8, duplicate keys at any depth (including escaped aliases) and trailing
JSON refuse. errors contain no payload. output is serialized into a bounded
buffer and depth-checked before its first byte is written, so oversized replies
do not emit a partial JSON frame. each reply is flushed. the local status producer
still owns fragment semantics; the host independently validates them before UI.

## evidence boundary

`examples/managed-status.rs` is an explicitly labelled protocol fixture, **not**
clock/system-status/pane-pwd or a managed package. its callback reports invocation
count, slot and the received terminal data so tests observe actual authority and
payload changes at child stdout. build it before all-target tests (also wired in
native CI). it is not packaged by the production release-bin build.

`tests/managed.rs` drives that executable and the same I/O loop. it covers current
and refused admission, stale binding/ID suppression, canonical scoped data,
framing and shutdown/EOF. these client-boundary tests are not real PluginHost
admission, graphical output, a managed production port, artifact validation,
timezone parity or migration acceptance. those gates remain separate and open.
`examples/managed-pages.rs` is a second labelled protocol fixture for shared-engine
count/byte continuation and failure tests, not another catalogue entry.
