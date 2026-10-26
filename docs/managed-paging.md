# SDK paging and local bounds

`PageConsumer`/`run_paged` extend the same API 1.2 admission, validation, framing
and dispatch loop used by clock, resources and pane-pwd. there is no second wire
protocol, host-source dependency, settings/event grant or transport fallback.

`status` consumes the initial page. `Continue(state)` asks the engine to enumerate
the exact returned string cursor in the same scope; `page` reduces each matching
`context_page`. `Complete(Status)` answers the original host-owned status ID.
enumeration IDs start at one, use a separate checked u64 sequence and never advance
the status-ID watermark. only one enumeration remains outstanding until its exact
session/generation/ID/scope match arrives. unsolicited, duplicate or mismatched
pages refuse before consumer invocation; they do not reset or retry traversal.

other live statuses are dispatched while a page is outstanding. pending traversals
are scheduled FIFO; queued time counts against each traversal's own deadline.
duplicate live scope or exhausted correlation capacity refuses rather than silently
dropping a request. stale status binding/IDs retain the existing ignore behavior.

## deadline and failure

each logical traversal gets one absolute monotonic **four-second local client
budget**, beginning when its initial status is admitted. pages/progress do not
renew it. this is not a new host/wire guarantee; transport time already spent on
the host is not refunded and the existing five-second host deadline is unchanged.

real stdio uses nonblocking descriptors and deadline-aware poll/read/write, including
partial frames, no newline, silent-open stdin and blocked stdout. buffered input
and dispatch are checked against the same earliest pending deadline. missing or
late continuation, timeout and exhausted bounds terminate nonzero. no empty
successful status, replay, cursor reset or enumeration retry is manufactured.
EOF while a traversal remains pending fails; explicit shutdown cancels and exits
without waiting for a page or EOF. diagnostics are one best-effort nonblocking
line of at most128 bytes containing only an error category, not payload/identity/path.

idle nontraversal admission still relies on the host's existing startup/lifetime
policy; this does not invent another wire timeout. reducers are trusted local
plugin code: they must not perform blocking work or hide unbounded page buffers
in their consumer object. the SDK cannot preempt arbitrary reducer CPU code.
existing host crash/restart/backoff/revocation accounting is not bypassed. client
exit is not stop acknowledgement or descendant/group-quiescence proof.

## retained-state derivation

these are enforced local bounds derived from existing API 1.2 frame/page ceilings,
not additional host protocol constraints:

| bound | derivation and units |
| --- | --- |
| pending correlations |15, the existing shared status/config credit ceiling; this SDK advertises no config consumer |
| reducer state |`Copy + Serialize + 'static`; physical type size and encoded state each at most28×1024=28672 bytes, the existing page ceiling |
| one retained record |binding, original ID, scope, cursor and state, measured without allocating serialized data; at most65536 bytes including its accounting newline |
| total encoded retained records |15×65536=983040 bytes (960KiB), checked before queue admission/replacement |

the pending table is preallocated for the fixed15-correlation ceiling only after
the state-type-size check. callback state and its envelope are measured with a
bounded counting writer before retention; no scope/page clone or growing page
collection is needed. input/output frame buffers reserve the existing64-KiB limit
and check length before extension. encoded budgets are not a claim about total
allocator/RSS overhead, JSON node representation or arbitrary consumer-owned data.

copy-only static state cannot own a `String`/`Vec` or borrow the input page. use a
small count/summary or fixed-size reducer, not concatenated pages. the supplied
fixture stores only a u64 count. pages reflect the current world, not a frozen
snapshot: creation/removal/moves can change observed records. the engine does not
invent cursor revisions, completeness, numeric ordering guarantees or retries.
target closure may yield no page; that expires nonzero rather than reusing old
context. publication on a closed/retired target remains the host's responsibility.

## evidence boundary

`managed-pages` is a labelled protocol-only example, not a plugin/package. direct
stdio tests cover application/workspace continuation and later-page-dependent
output, interleaving, correlation, shutdown/EOF, missing/partial/late pages,
absolute deadline and resource refusal. production pane targeting remains a
separate real entrypoint with only terminal/cwd/status grants. direct protocol
tests do not establish authenticated installation, real PluginHost/GTK lifecycle,
native arm64 or installed-package acceptance.
