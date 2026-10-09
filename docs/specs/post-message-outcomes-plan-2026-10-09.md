# Plan: expose post_message destination outcomes

Status: ready for independent Plan Review. Question_1791584165_960437 approves the nonblocking inbox-result direction. Implementation still requires Plan Review.

## Target and authority

- Ticket: ticket_1791583754_444241.
- Run: run_1791583785_336563.
- Target: tgt_83619444571645afa5507374e36036e2.
- Repository: Tonksthebear/trybotster.
- Inspected revision: e572e581db9208299130493fad108d5557ea3ae8.
- Registry source: list_spawn_targets. The registry maps this target to the legacy trybotster repository.
- Repository authority: AGENTS.md and CLAUDE.md, as confirmed by question_1791583858_644587.
- Repository playbook: [[legacy-trybotster-monorepo-architecture]] supplies legacy context. No modular repository charter applies. The supplied routing map omits this legacy target.

The source directory of the reporting agent does not select the MCP implementation. The independent routing answer is question_1791583776_211720. Artifact artifact_1791583894_837183 records the routing decision.

README.md describes the legacy runtime and catalog. Mainline already places plans in docs/specs. This document follows that placement.

## Loaded context

Role notes: [[planner-playbook]] and [[botster-planner-playbook]].

Runtime and repository context: [[botster-architecture]], [[legacy-trybotster-monorepo-architecture]], [[cli-patterns]], [[rails-conventions]], and [[spa-patterns]]. CODEX.md, the vault CLAUDE.md, identity, goals, and the session-start conventions also apply.

Targeted notes:

- [[botster plugin runtime uses supervisor plus per plugin workers]]
- [[plugin mcp handlers run in plugin worker vms]]
- [[plugin agent spawns are queued parent hub commands]]
- [[botster plugin workers must enqueue parent hub writes]]
- [[botster plugin worker parent routing follows invocation mode]]
- [[botster plugin mcp timeouts must cover child operation bounds]] — superseded; do not use its larger deadlines.
- [[engineering-build-and-test-operations]]
- [[botster runtime teardown lenses]] — assessed below.
- [[project pipeline orchestration belongs in a device-level botster plugin]]
- [[project pipelines needs an operator workbench not more primitives]]
- [[project pipelines ui contract belongs in the plugin readme]]
- [[botster orchestration should spawn agents with explicit target ids]]
- [[botster orchestration prompts must bind agents to explicit worktrees]]

The orchestrate-delivery skill and its Botster lessons require production-path proof and explicit ownership. The Project Pipelines package playbook is not applicable: this ticket changes neither its package nor workflow policy.

## Source findings

1. list_plugins identifies the loaded messaging worker as the device plugin at .botster-dev/plugins/messaging/init.lua, version 1.2.0.
2. The repository catalog template is version 1.1.0. Both post handlers call Hub:post without wait. Other template differences are outside this repair.
3. catalog/templates/plugins/messaging/init.lua registers post_message. An explicit session_uuid bypasses label lookup.
4. cli/lua/lib/hub.lua maps the worker's omitted hub_id to the parent. Its worker branch enqueues post_message and returns queued.
5. cli/src/lua/primitives/plugin_worker.rs creates an enqueue response channel and discards the receiver. The parent result cannot reach that original return value.
6. cli/src/hub/server_comms.rs handles PluginWorkerParentRequest through service_parent_request. Hub._handle_worker_parent_request invokes local Hub:post.
7. Local Hub:post checks Agent.get, rejects missing sessions and accessories, writes agent._inbox, and sends a PTY doorbell.
8. Agent inherits the live Session registry. Session:close unregisters the session, kills its PTY, removes its registry entry, and records canonical closure.
9. receive_messages drains only the authenticated caller's inbox. Agent.receive_messages removes expired envelopes and clears the inbox. Recovery initializes an empty inbox.
10. Remote posting uses a destination request. The destination registry is authoritative. Omitted hub_id does not search other hubs.
11. remote_request currently reconnects and repeats some failed requests. A mutating post can have an uncertain first outcome. This path needs a no-duplicate check.

Existing MCP callbacks complete when the worker handler returns. They do not represent completion of an enqueued parent post. The shared worker command queue logs later failures; it does not expose a post result.

## Product contract and scope

The sender must distinguish command admission from destination acceptance. Known absent and retired destinations must produce an observable refusal correlated with the original post. A live destination must still receive its payload.

Preserve nonblocking worker-parent writes. Question_1791583972_793918 rejects opts.wait=true for normal post_message and rejects timeout increases. This decision supersedes any earlier direction that implied synchronous waiting.

In scope: the messaging MCP result contract, parent post completion, caller correlation, retirement races, remote uncertainty, focused documentation, and regression evidence.

Out of scope: modular Hub changes, transport redesign, new review policy, automatic retries, deployment, broad plugin cleanup, and unrelated template differences. The bug-report plugin remains stateless.

No external repository dependency is established. If implementation finds an external prerequisite, create its ticket against that repository's target and register the dependency before expanding work.

## Approved asynchronous API direction and exact proposed schema

The immediate worker response is {msg_id, status: queued_unverified}. It proves command admission only.

After destination processing, the parent writes one system result envelope to the original caller's existing inbox. The caller obtains that envelope through receive_messages. The envelope uses reply_to for the original msg_id. Its payload identifies the destination hub, destination session UUID, and outcome.

The completion envelope uses type `post_message_result`. Its `reply_to` is the original message ID. Its own `msg_id` is a separate envelope ID. Its `from` identifies the originating Hub and the reserved system actor `hub`. Its payload has these fields:

```json
{
  "msg_id": "original message ID",
  "destination": { "hub_id": "destination Hub ID", "session_uuid": "destination UUID" },
  "status": "no_such_session",
  "admission": "rejected",
  "retry_safe": true,
  "error": { "code": "no_such_session", "message": "The destination session does not exist." }
}
```

`error` is absent for ordinary success. Human-readable error text is explanatory; callers branch on `status`, `admission`, and `error.code`.

| status | admission | retry_safe | Meaning |
| --- | --- | --- | --- |
| delivered | accepted | false | Inbox write and doorbell call completed. |
| inbox_only | accepted | false | Inbox write completed, but no PTY exists or the doorbell failed. A failure uses error.code=doorbell_failed. |
| duplicate | accepted | false | The same original msg_id remains in the destination inbox. |
| no_such_session | rejected | true | The destination registry contains no session at admission. |
| invalid_target | rejected | true | The destination exists but cannot accept inbox messages. Use error.code=not_an_agent. |
| rejected | rejected | true | A proved pre-admission failure prevented the inbox write. Use error.code=admission_failed. |
| unverified | unverified | false | Processing cannot be established. Use error.code=transport_unavailable, transport_timeout, or outcome_unknown. |

The implementation must track the admission point explicitly. A thrown error after the inbox write cannot become rejected. If write progress is unknown, return unverified. `retry_safe` describes duplicate risk for this attempt; it does not instruct automatic retry.

Reserve `post_message_result` for the internal completion path. Reject that public msg_type before enqueue so a caller cannot impersonate a system result. Verify the reserved sender marker and original destination when consuming results.

Internal result envelopes do not request another result. The authenticated caller context supplies the result recipient; caller input must not select an arbitrary recipient. Existing caller-only inbox access remains intact.

Delivered means the destination wrote the inbox and issued the doorbell. It does not prove receive_messages, human attention, or review completion. Inbox_only means the inbox write completed without a PTY doorbell. Duplicate retains the existing same-msg_id meaning, limited to envelopes still present in the inbox.

The destination performs lookup and admission together. A sender-side existence check is not authoritative. If close completes first, reject the post. If admission completes first, report only the completed write; later close can still prevent consumption.

Use the existing envelope expiry contract, including its default 3600 seconds. Results remain memory-backed. Restart, sender retirement, expiry, prior drain, or absent completion leaves an unresolved caller without proof. Do not convert missing evidence into success or definite failure.

No automatic retry follows an uncertain outcome. An explicit refusal proves that this attempt did not enter the destination inbox. Callers can correct that refusal and submit a new attempt. Do not claim durable idempotency from the current inbox-only duplicate check.

Reachable remote destinations retain authoritative results. Disconnection or timeout remains unverified. Preserve the original correlation across the remote path. Do not repeat an ambiguous mutating request automatically.

Compatibility implication: callers must distinguish the immediate acknowledgment from a second result envelope. Update the tool description and add catalog/templates/plugins/messaging/README.md with examples. Inventory in-repository callers before editing them. Direct Hub:post callers retain their existing opt-in wait or queue contract; do not impose completion envelopes on unrelated plugin writes. Document external caller migration without editing shared installed plugins.

The public messaging tool opts into completion internally. Callers cannot choose its result recipient. For a worker-local post, the existing parent command carries the original message ID and authenticated caller context. The parent performs the write and result insertion without a worker wait. Catch the post outcome before the discarded Rust response channel; do not retain a receiver merely to block on it.

For remote posts, reuse hub_client.send and its on_message path with the existing _mcp_rid correlation. The orchestrator RPC already echoes _mcp_rid. Extend the owning response dispatcher without replacing another subscriber. Keep transient correlation until response, disconnect, or the existing remote request deadline. Release that state after completion. No polling or durable status store is added. Do not call blocking hub_client.request on the parent control loop. Carry msg_id through the destination RPC and disable replay of this mutating operation. If the callback owner cannot support this route without broader redesign, return the concrete blocker to Plan before implementation continues.

## Implementation sequence

1. Have independent Claude Plan Review validate the approved API direction, exact proposed schema, ownership, compatibility, and nonblocking behavior.
2. Add a regression through the real messaging tool loaded in a plugin worker. Observe the parent result and the caller inbox through the real consumer.
3. Reuse the existing envelope and parent dispatch mechanisms for the approved result path. Keep messaging policy in the messaging surface. Keep registry authority in the parent.
4. Catch authoritative refusals at the result boundary. Preserve stable correlation and original destination identity. Do not derive typed errors from fragile string matching.
5. Preserve the enqueue path for normal worker posts, agent creation, and notifications. Do not turn the parent control thread into a remote blocking waiter.
6. Document result semantics and retry limits beside the messaging template. Change Rust only where the approved completion route requires it.
7. Run focused and repository gates. Submit raw evidence and exact revisions for independent Review and Verify.

Likely files: catalog/templates/plugins/messaging/init.lua, catalog/templates/plugins/messaging/README.md, cli/lua/lib/hub.lua, cli/tests/internal_client_lua_test.rs, and cli/tests/plugin_worker_runtime_test.rs. Inspect catalog/templates/plugins/orchestrator/init.lua for remote correlation. cli/src/lua/primitives/plugin_worker.rs, cli/src/lua/primitives/hub_client.rs, cli/lua/lib/mcp.lua, and cli/src/mcp_http.rs are completion owners to inspect, not blanket edit authorization.

## Acceptance and evidence

| Case | Required observation |
| --- | --- |
| Never-created local UUID | Immediate acknowledgment says unverified. The correlated result says no_such_session. No destination inbox is created. |
| Retired local UUID | Exercise the production close path, then post through MCP. Observe correlated refusal. A stale sender list must not permit success. |
| Live target | Receive the original payload through the target's receive_messages. The sender receives the matching result. Verify doorbell behavior. |
| No PTY | Preserve inbox_only where the existing live registry permits this state. Do not claim a doorbell. |
| Wrong session type | Preserve accessory refusal with a typed result. |
| Close races | Drive close-before-admission and admission-before-close deterministically. Check the result against the destination operation order. |
| Caller identity | Another caller cannot consume the sender result. A spoofed recipient field cannot redirect results. |
| Spoofed completion | Public callers cannot send the reserved result type or choose its system sender. |
| Doorbell failure | Force failure after the inbox write. Observe accepted/inbox_only with retry_safe=false, never a safe refusal. |
| Result recursion | One post produces at most one completion envelope. Completion envelopes produce no further completion. |
| Remote live/missing/retired | Exercise the real destination handler. Verify destination authority and correlation across hubs. |
| Remote unavailable or timeout | Return explicit uncertainty. Prove no automatic duplicate post after an ambiguous transport failure. |
| Restart, expiry, sender close | Do not fabricate success. Verify result loss follows the documented memory-backed contract. |
| Responsiveness | Hold parent processing. The post handler must return without waiting. An unrelated messaging invocation and healthy sibling session must still progress. |

Use the actual asynchronous MCP entry path: tools/call -> mcp.call_tool -> plugin worker -> parent Hub event -> Hub:post -> target receive_messages and sender result consumption. A mocked Hub:post alone is insufficient.

Keep synchronous fallback-route coverage and asynchronous event-route coverage. Existing responsiveness tests are positive controls. Use deterministic barriers rather than sleeps to prove operation order.

From cli/, use ./test.sh --integration --test internal_client_lua_test and ./test.sh --integration --test plugin_worker_runtime_test for focused coverage. The wrapper builds the release binary before integration tests. Run ./test.sh --all -- --test-threads=1 for the final CLI gate. Do not run raw cargo test. Add affected template tests if present or introduced.

The inspected CI uses Zig 0.16.0, submodules, Rust stable, and BOTSTER_ENV=test. Its raw Cargo commands do not override the local wrapper requirement. Rails and frontend gates apply if their files change; no such change is planned.

Independent Verify must reproduce the targeted failure on the inspected base and verify the candidate. Preserve raw output, exit codes, selected test counts, source revision, binary revision, and plugin hash. A setup failure is not a failing regression. Reproduce unrelated-failure claims on the same base.

Run an isolated live legacy Hub with the candidate binary and template. Use disposable sessions for absent, retired, live, and remote cases. Record the loaded plugin identity. Do not reload or deploy the shared host as part of this ticket.

The Plan worktree has a nonempty tracked .gitignore and no colon in its path. Every later gate must recheck both conditions. Restore .gitignore from HEAD if required. Set a colon-free CARGO_TARGET_DIR when a colon-bearing legacy worktree requires it.

## Runtime teardown assessment

teardown_class_applies: no for the proposed change. This repair changes message acknowledgment and observes the existing close path. It does not change SessionIo, ClientWorker, peer cleanup, runtime ownership, or resource teardown.

The loaded teardown note was checked because the ticket requires retirement-race proof. If the implementation changes teardown rather than only observes it, stop and revise the plan with every required lens. Do not infer stopped runtimes from a removed registry row.

## Risks, assumptions, and open decisions

- Question_1791584165_960437 approves the API direction for independent Plan Review. Review must assess the exact schema above; no implementation or test waiver is implied.
- Source proves the queued-result mechanism. The historical six sends cannot be replayed without the missing retired UUID and original logs.
- Extra result envelopes can create caller noise. Their type and correlation must be clear and must not create recursive sends.
- Parent errors, transport errors, and post-write doorbell failures have different certainty. Classification must preserve that difference.
- Remote processing must not block the parent control loop. Existing request mechanisms need source and runtime review before selecting the remote completion route.
- The loaded device template differs from the catalog. Verification must identify the tested combination rather than assume installed parity.
- No session-type eligibility parent applies. Modular Hub pins, hub-test-support 0.1.26, and conf 33 are not dependencies of this legacy repair.
- No SPA state changes apply. If scope changes, retain the required request-race and browser-state proof.

## Vault discipline

Exactly one checklist exists for this Plan visit: checklist_1791583871_139488. No duplicate was created.

Capture candidates: worker enqueue acknowledgment does not prove parent acceptance; legacy MCP ownership follows the loaded plugin rather than the sender directory; missing legacy routing requires an explicit authority decision. The asynchronous direction is approved for review, not shipped. Preserve that distinction in the capture.

At delivery completion, run document, connect, update, and verify for the new capture. Record convention conflicts, final verification, and capture paths in the checklist.
