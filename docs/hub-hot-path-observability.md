# Hub Hot-Path Observability

The Rust hub records compact diagnostics for event-loop paths that can make a
client look connected but stale: socket messages, WebRTC messages, client
terminal subscriptions, session-I/O control responses, OSC/timer volume,
terminal snapshots, manifest writes, socket repair, cleanup, and reconnect
handling.

These diagnostics are intentionally local to the daemon. They use existing
structured log lines and in-process `HubEventMetrics`; there is no external
metrics backend, browser UI, Catalyst surface, Elements component, or
`tmp/tailwind_plus_preview` dependency for this work.

## Primary Log Surface

The periodic hub event log includes aggregate queue health, per-event-kind
handler timing, ad hoc counters, hot-path spans, and bounded slow samples:

```text
[HubEventMetrics] enqueue_ok=... dequeue=... failed=... pending=... pending_hwm=... bytes_pending=... bytes_hwm=... avg_us=... max_us=... by_type=[...] counters=[...] spans=[...] slow_samples=[...]
```

Use this line first when debugging data-plane stalls. `by_type` measures the
whole event handler for a hub event kind. `spans` measure named sub-paths inside
handlers and are not guaranteed to be disjoint. For example,
`webrtc_message.total` covers the full WebRTC message dispatch and overlaps
typed spans such as `webrtc_message.dc_ping`, `webrtc_message.dc_pong`,
`webrtc_message.terminal_color_profile`, and `webrtc_message.lua`.

Slow samples are capped in memory and in logs: the hub keeps the most recent 32
slow samples, then logs the slowest 8 from that bounded set. Slow-sample labels
are capped to 24 characters so peer/session identifiers do not make log lines
unbounded.

## Hot-Path Spans

The most useful span names are:

| Span | Meaning |
|------|---------|
| `socket_message.focus_changed` | focus-change socket handler |
| `socket_message.terminal_color_profile` | terminal color profile socket handler |
| `socket_message.lua` | Lua fallback for socket messages |
| `webrtc_message.total` | full WebRTC message parse and dispatch |
| `webrtc_message.parse_json` | JSON parse for WebRTC messages |
| `webrtc_message.dc_ping` | DataChannel ping handler |
| `webrtc_message.dc_pong` | DataChannel pong handler |
| `webrtc_message.terminal_color_profile` | WebRTC color profile handler |
| `webrtc_message.lua` | Lua fallback for WebRTC messages |
| `webrtc_send.queue` | queued WebRTC send item byte accounting |
| `client_worker.backpressure` | client worker queue rejected delivery/control work |
| `cleanup.webrtc_scan` | periodic WebRTC channel cleanup scan |
| `cleanup.webrtc_channel` | cleanup of one WebRTC channel |
| `snapshot.rpc_get` | blocking terminal snapshot retrieval |
| `snapshot.gzip_queue` | snapshot gzip and queue preparation, emitted while the session I/O data-plane helper prepares transport bytes |
| `manifest.write` | hub runtime manifest write from hub-owned call sites |

Hot socket/WebRTC subhandlers and cleanup scans are slow at 50ms. Snapshot RPC
and gzip/queue spans are slow at 100ms. Manifest writes are slow at 10ms.

Session-process PTY output no longer reaches the hub as a hot-path byte event.
The session I/O worker coalesces durable-session output and fans it to
subscribed client workers; the hub records attach, detach, snapshot, lifecycle,
and backpressure policy. WebRTC transport queues are owned by
`WebRtcPeerRegistry` and enter the hub as typed control events or adapter
commands. Snapshot byte preparation lives behind `worker::session_io` helpers,
but the stable `snapshot.rpc_get` and `snapshot.gzip_queue` span names remain
unchanged for operator playbooks.
Backpressure recovery snapshot delivery records the same gzip queue span and
the `snapshot.backpressure_recovery.*` counter family so recovery paths remain
visible even when normal hot PTY delivery is congested.

## Counters

Counters are cumulative within the daemon process. High-water counters keep the
largest observed value instead of summing every sample.

| Counter family | Meaning |
|----------------|---------|
| `socket_message.error` | socket Lua callback errors |
| `webrtc_message.parse_error` | WebRTC JSON parse failures |
| `webrtc_message.lua_error` | WebRTC Lua callback errors |
| `webrtc_send.queued` | send item queued for a peer |
| `webrtc_send.full` | per-peer send queue full |
| `webrtc_send.closed` | send queue closed |
| `webrtc_send.dead_peer` | send detected a dead peer |
| `webrtc_send.unknown_peer` | send targeted a peer with no active send task |
| `webrtc_send.unknown_peer_burst` | unknown-peer burst guardrail fired |
| `webrtc_channel.closed_after_connect` | channel closed shortly after connected/open |
| `client_worker.backpressure` | terminal/client worker queue rejected work |
| `client_worker.session_io_missing` | terminal input or resize arrived without a registered session I/O sender |
| `pty_osc.title`, `pty_osc.cwd`, `pty_osc.prompt`, `pty_osc.cursor`, `pty_osc.other` | OSC subtype volume |
| `pty_osc.volume_burst` | OSC volume guardrail fired |
| `timer_fired.count` | timer events fired |
| `timer_fired.volume_burst` | timer volume guardrail fired |
| `snapshot.empty` | snapshot request returned no bytes |
| `snapshot.queue_full` | snapshot queue was full |
| `snapshot.queue_closed` | snapshot queue was closed |
| `snapshot.backpressure_recovery.sent` | recovery snapshot sent |
| `snapshot.backpressure_recovery.empty` | recovery snapshot was empty |
| `snapshot.backpressure_recovery.failed` | recovery snapshot failed |
| `cleanup.webrtc.reason.*` | WebRTC cleanup reason counts |
| `cleanup.webrtc.duplicate_skipped` | duplicate cleanup was ignored |
| `reconnect.pending` | largest pending reconnect set seen in a cleanup tick |
| `reconnect.retry`, `reconnect.expired`, `reconnect.ready`, `reconnect.stale_generation`, `reconnect.failed` | reconnect state transitions |
| `socket_path.repair` | missing hub socket path repair attempt |
| `socket_path.repair_error` | socket path repair failed |
| `manifest.write_error` | manifest refresh failed at a hub-owned call site |

For session-process output, observability is intentionally control-plane
oriented: client-worker backpressure, missing session I/O sender counters,
snapshot counters, reconnect counters, and WebRTC queue counters show where the
system is congested without routing durable-session PTY bytes through hub event
handlers.

## Guardrail Logs

Guardrails diagnose bursts without changing data-plane behavior.

Unknown-peer sends are tracked in a 30s rolling window with a 16 peer-prefix
cap. When one peer prefix reaches 10 unknown sends in the window, the hub logs
once for that prefix/window and increments `webrtc_send.unknown_peer_burst`:

```text
[WebRTC-Guardrail] event=unknown_peer_burst peer=... count=... window_ms=30000
```

If a DataChannel closes within 10s after reaching connected/open state, cleanup
increments `webrtc_channel.closed_after_connect` and logs:

```text
[WebRTC-Guardrail] event=closed_after_connect peer=... reason=... connected_age_ms=...
```

Immediately after deploying the connected-at tracking, a long-lived channel
that predates the new state can be backfilled during cleanup and may produce a
one-off `closed_after_connect` warning if it closes soon after that rollout.
Treat that first warning as diagnostic evidence to correlate with surrounding
logs, not proof of a new reconnect regression by itself.

OSC and timer bursts are tracked in a 30s rolling window. Once a subtype exceeds
1000 events in that window, the hub logs once for that subtype/window and
increments the matching `*.volume_burst` counter:

```text
[HubEvent-Guardrail] event=volume_burst subtype=... count=... window_ms=30000
```

Manifest writes log from `cli/src/hub/daemon.rs` even when no `HubEventMetrics`
handle is available. More than 3 writes in a 10s window emits one warning for
that hub/window:

```text
[ManifestMetrics] event=write hub=... elapsed_ms=... bytes=...
[ManifestMetrics] event=write_storm hub=... count=... window_ms=10000
```

Runtime artifact cleanup emits local timing logs:

```text
[RuntimeArtifactsMetrics] event=cleanup_stale_files hub=... elapsed_ms=...
```

## Verification

For CLI changes, use the repo test script instead of raw `cargo test`:

```bash
cd cli
./test.sh --unit
```

The original instrumentation slice was verified with `1437 passed; 0 failed; 1
ignored`. The session-I/O worker paste/snapshot slice was verified with `1465
passed; 0 failed; 1 ignored`. Direct tests cover `HubEventMetrics`
counters/spans/slow-sample bounding, unknown-peer burst rate limiting and
peer-prefix caps, PTY output batch metrics, and session-I/O worker output
coalescing. Snapshot
preparation and session-scoped paste-file writes have focused worker unit
coverage.

Workerized data-plane recovery tests now embed minimal reproductions of observed
Botster daemon log failure shapes: 1001 noisy PTY frames with OSC traffic,
`pty_osc.cursor` volume bursts over the 30s guardrail window, session-reader
EOF cycles, WebRTC reconnect/backpressure churn, and slow-client recovery
snapshot delivery. The hub-side replay test records `session_io_batch` handler
timing through `HubEventMetrics` and asserts p99 handler time stays under the
existing hot-subhandler budget with no slow samples. This is the
preferred regression shape for future changes to session I/O batching, OSC
guardrails, WebRTC recovery routing, or snapshot gzip queueing.

Queue/counter paths and manifest write-storm rate limiting are not exhaustively
unit-tested; use manual log verification or add focused tests when changing
those paths. The current full CLI unit verification for the data-plane load and
recovery slice passed with `1484 passed; 0 failed; 1 ignored`.
