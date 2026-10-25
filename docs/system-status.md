# system-status: explicit managed entrypoint, unchanged real sampler

`system-status-managed` is the production managed API 1.2 entrypoint for identity
`github:1431055733/system-status`. it requires only `status`, selects no optional
features and uses the shared [managed transport](managed-status.md). legacy hello
and pre-admission status are not fallback admission paths. the original
`system-status` entrypoint and its original regression tests retain legacy 1.0.

both entrypoints keep one `Sampler<Linux>` and one monotonic start time for their
whole process lifetime. the typed application-target adapter calls the same
`Sampler::status`; it does not reconstruct the sampler, copy the calculations or
turn a deterministic test source into the production measurement path.

## output, failure and authority

the application header returns three fragments, priorities 10/11/12:

- **CPU**: aggregate busy time between the first eight `/proc/stat` CPU counters;
  idle and I/O wait are excluded, and guest fields are not counted twice.
- **MEM**: used percentage from `MemTotal - MemAvailable`, not process RSS or a
  fabricated constant.
- **DISK**: `statvfs("/")`, df-style used blocks divided by used plus available
  blocks, rounded upward. reserved availability is not counted as user-free space.

successful percentages are in `0..=100`; their numeric meter is exactly the displayed
percentage divided by 100. unknown values are `N/A` with `meter: null`, not zero.
the first CPU response has no interval and is therefore N/A. an unavailable or
invalid source affects its own field. CPU failure clears its prior sample: the
first successful read afterwards still needs another sample to form an interval.

CPU and memory are sampled no more than once per second; disk no more than once
per five seconds. responses request host polling with `refresh_ms: 1000`, not an
unsolicited stream. repeated requests inside a sampling interval reuse the cached
values. application-footer and defensive wrong-target/slot calls produce no
fragments and do not sample or advance the clocks. actual pane/workspace requests
and ungranted context fields are refused by managed transport before the callback.
no terminal/cwd/settings/notification access is needed or selected.

these are the **visible aggregate proc view and inherited root mount** of the
plugin process. in a container, `/proc` may expose host-wide or virtualized
accounting while `/` names the container mount. the values do not promise host-
global metrics, per-plugin usage, cgroup entitlement or CPU-quota normalization.
the host is not a sandbox and this port adds no timezone/environment or OS grants.

## evidence boundaries

new stdio tests select the actual `system-status-managed` executable. besides
strict admission/binding and target refusal, they require initial CPU N/A followed
by real CPU/MEM/DISK percentages. an independent test parser brackets both CPU
observations; excessive scheduling skew fails instead of broadening tolerance or
retrying until values agree. memory is read independently around the request.
bounded, child-locally configured `/usr/bin/df -P /` observes disk near its initial
sample, not after the five-second cache has legitimately retained an older value.
comparison tolerances are 2/2/1 percentage points for CPU/MEM/DISK; meter/text
agreement and bounds are checked independently of those tolerances.
the unchanged full suite stays mandatory. native CI adds one exact successful-
output capture afterwards, recording source, executable digest and numeric/skew
observations. this is not a retry-until-green or a substitute for the full suite;
the extra invocation is not counted as another distinct integration test.

separate injected-source tests exercise rates, unsupported slots, each source's
independent N/A and recovery. they supply valid `0..=100` source percentages; the
`Option<u8>` source interface itself does not express that numeric invariant.
these deterministic tests are not live Linux measurements or grant enforcement.
shutdown requires exit while stdin is still open; EOF is a separate positive
case. invalid unterminated frames must fail before EOF at the wire limit.

current-head execution is required: writing tests is not proof they have passed.
client output and numeric meters do not establish actual host graphical bars,
managed installation/review/enablement, update/disable/uninstall, terminal
continuity or whole task 2.3 acceptance. those require exact-source host consumers
and their real applicable boundary checks. no manifest/catalogue/artifact or
plugin release is supplied here; future task 2.5 must explicitly select
`system-status-managed`, without guessing the legacy executable from the id.
