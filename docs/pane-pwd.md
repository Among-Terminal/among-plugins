# pane working directory

`pane-pwd-managed` is the explicit managed API 1.2 entrypoint, identity
`github:1431055733/pane-pwd`. it requires `status`, `context.terminals` and
`context.cwd`, with no optional features, settings, events or notifications.
missing any required grant refuses admission. discovery/building does not install,
review or enable the plugin; no installable release is claimed here.

## target and rendering

only `pane_footer` with a pane target emits a directory. the adapter matches the
exact canonical string reference supplied by Among, never a display index, focused
pane, sibling, process directory or environment fallback. other valid slots return
an empty status with the same one-second refresh. malformed references, sibling
data and undeclared fields refuse in the shared transport before rendering.

the path comes from host-mediated context, not filesystem inspection by this
process. missing target data, absent/null/empty cwd and null with
`unavailable:["cwd"]` produce no fragment. closed-target publication authority is
the host's responsibility; client validation is not evidence that a target is
still live when its reply reaches Among.

legacy `pane-pwd` remains on protocol 1.0 with numeric pane lookup. both entrypoints
share the known-path renderer: first 480 Unicode characters in the visible text,
the entire accepted path in the tooltip, priority 10 and 1000-ms refresh. neither
entrypoint sniffs or falls back to the other's protocol.

## bounds and unfinished paging

the shared transport limits cwd to 4096 UTF-8 bytes and 8192 JSON-encoded bytes,
pages to 32 terminals/28 KiB, and frames to 64 KiB including newline. it rejects
over-limit raw input rather than truncating it. Among can project an unavailable
value as null before it reaches this process; the display's 480-character cut is
separate from these authority/transport bounds.

a normal pane projection contains at most one terminal and needs no continuation.
**shared SDK `enumerate`/`context_page` continuation is not implemented yet.** this
initial-page adapter does not close the paging requirement: separate application/
workspace continuation tests and an agreed missing-page/exhaustion policy remain
required. it grants no application-wide access merely to manufacture pagination.
see the [shared transport contract](managed-status.md).

`tests/managed_pwd.rs` exercises the actual entrypoint's stdio. these are client
boundary tests, not installed PluginHost/GTK lifecycle, hidden live-pane, closed
target, package, legacy-retention or complete paging acceptance.
