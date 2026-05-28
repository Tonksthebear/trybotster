# Stable Webhook URL Contracts

Status: scaffold-only implementation contract. This document defines the
contracts for future Rails, CLI, and plugin implementation slices. It does not
mean a production runtime path exists yet.

## Goal

Botster needs durable public webhook URLs without leaking Cloudflare account
credentials or pushing provider policy into core. The contract has three
separate platform owners plus consumer/provider plugins:

| Layer | Owns | Does not own |
|---|---|---|
| Rails broker | Cloudflare account credentials, named-tunnel desired state, hostname allocation, connector token minting, rotation, revocation, and hub auth | provider webhook parsing, consumer plugin secrets, local handler execution |
| Cloudflare stable-url hub plugin | per-hub connector lifecycle, token secret pointer, local cloudflared config, URL claim pool, reconciliation, and entity publication | Cloudflare account API token, consumer webhook verification, arbitrary path multiplexing |
| `local_webhooks` primitive | provider-neutral local HTTP listener, route registration, bounded request delivery, response shaping, reload cleanup, and generation fencing | Cloudflare policy, signature verification, replay/idempotency semantics, durable business processing |
| Consumer/provider plugins | stable URL claims, provider signature verification, event parsing, replay/idempotency, and durable domain actions | Cloudflare tunnel token, Rails account credentials, listener process ownership |

## Runtime Status

No production entry point changes in this ticket. Follow-up implementation
slices must prove runtime behavior through:

- Rails controller/model/adapter tests that exercise the broker endpoints.
- CLI tests that send a local HTTP request through `local_webhooks` into a
  plugin worker mailbox.
- Lua/plugin tests that claim a stable URL, publish entities, store only secret
  pointers, and reconcile a single connector config.
- Reload tests that prove route generations fence stale completions.

The current Cloudflare hosted-preview quick-tunnel plugin remains separate. It
surfaces temporary preview URLs and is not the stable webhook URL connector.

## Rails Broker API

Rails is the only component that knows Cloudflare account credentials. It uses
hub-scoped authenticated API routes and returns only hub-scoped connector
material.

Future route shape:

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/hubs/:hub_id/cloudflare_tunnel` | Return desired tunnel state, assigned hostnames, token generation metadata, and connector status. |
| `POST` | `/hubs/:hub_id/cloudflare_tunnel` | Ensure the hub has one named tunnel and one connector token. |
| `PATCH` | `/hubs/:hub_id/cloudflare_tunnel/rotate` | Mint a new connector token generation and revoke the old generation after handoff. |
| `DELETE` | `/hubs/:hub_id/cloudflare_tunnel` | Revoke connector tokens and remove desired hostnames for the hub. |
| `POST` | `/hubs/:hub_id/stable_webhook_hostnames` | Allocate or return a hostname from the hub pool. |
| `DELETE` | `/hubs/:hub_id/stable_webhook_hostnames/:id` | Remove one hostname from desired state after release or admin action. |

Authentication follows the existing hub bearer-token pattern. Responses never
include Cloudflare account credentials. Connector-token delivery is allowed only
to the authenticated hub and must be represented with `token_version`,
`delivered_at`, and `expires_at` or equivalent metadata. Rails should avoid
persisting raw connector tokens; if offline redelivery requires persistence, the
field must be encrypted and never rendered in JSON, logs, plugin entities, or
generated config.

Rails model names for future implementation should stay direct:
`HubCloudflareTunnel` for one per-hub tunnel and `StableWebhookHostname` for
hostname allocation. Model callbacks/concerns are acceptable Rails structure
for desired-state reconciliation. Do not add service-object sprawl for
orchestration that naturally belongs on those models or small adapter POROs.

## Cloudflare Lifecycle

Each hub has one Cloudflare named tunnel and one active connector token
generation. Multiple stable hostnames route through that tunnel.

The stable-url plugin writes one named-tunnel config file with an ingress array:
one rule per claimed hostname forwarding to the local listener URL, followed by
the mandatory `http_status:404` catch-all. It must not run `cloudflared tunnel
run` with `--url`, because that collapses config-file ingress into single
service mode.

Ingress service ports must match the actual local listener port allocated at
runtime. A Foreman-managed service must use the real assigned port, not
`${PORT}` or a guessed base port.

DNS registration and ingress routing are separate desired-state items. Rails
owns Cloudflare DNS/tunnel desired state. The hub plugin owns the local
connector process and reports observed state back through plugin entities.

## Hub Token Delivery And Storage

The hub plugin receives a connector token only from Rails, immediately stores it
through encrypted hub/plugin secrets, and keeps only a secret pointer plus
`token_version` in plugin state.

Forbidden surfaces:

- plugin entities
- `plugin.db` rows except secret pointers and generation metadata
- cloudflared config examples
- hub logs
- consumer plugin config
- browser/TUI payloads
- command environment dumps

Consumer plugins never receive Cloudflare tunnel tokens. They claim a URL and
register a local webhook route; the Cloudflare connector and `local_webhooks`
own transport plumbing below that boundary.

Rotation creates a new Rails token generation, writes the new token to hub
secrets, restarts or reloads cloudflared with the new secret, then revokes the
old generation after a successful connector health report. Revocation deletes
the hub secret, stops the connector, removes desired hostnames, and marks
claims unavailable until reconciliation restores capacity.

## Stable URL Claim Model

The Cloudflare stable-url plugin owns `stable_urls.claim`, `stable_urls.release`,
and `stable_urls.list` as Lua-facing APIs. A v1 claim owns an entire hostname.
Only one active owner may claim a URL at a time.

Claim input:

```lua
stable_urls.claim({
  owner_plugin = "github",
  owner_key = "repo:owner/name",
  purpose = "webhook",
  local_service_url = "http://127.0.0.1:47123",
})
```

Claim output:

```lua
{
  id = "surl_...",
  hostname = "hook-abc.example.invalid",
  public_url = "https://hook-abc.example.invalid",
  owner_plugin = "github",
  owner_key = "repo:owner/name",
  status = "claimed"
}
```

The claim model rejects path multiplexing in v1. A future contract may add
multiple path owners under one hostname, but until then path sharing is a
security and lifecycle ambiguity.

Claims persist in plugin-owned state so reload and daemon restart can reconcile
the connector. Release removes the owner, removes or disables the ingress rule,
publishes an entity update, and keeps enough audit state to explain recent
claim/release events.

## Stable URL Entity Contract

The stable-url plugin publishes `cloudflare-stable-urls.stable_url` as the
read-model family for browsers and TUIs.

Public fields:

| Field | Meaning |
|---|---|
| `id` | Stable claim id. |
| `hostname` | Assigned public hostname. |
| `public_url` | `https://` URL for the hostname. |
| `status` | `available`, `claimed`, `reconciling`, `unhealthy`, or `revoked`. |
| `owner_plugin` | Claim owner plugin key, or `nil` when available. |
| `owner_key` | Owner-scoped claim key, or `nil` when available. |
| `local_service_url` | Local listener URL without secrets. |
| `token_version` | Non-secret connector token generation. |
| `last_checked_at` | Last reconciliation/health observation timestamp. |
| `message` | Short non-secret status text. |

Never publish `token_secret_key`, raw token bytes, Cloudflare account
credentials, raw `config.yml` contents, cloudflared process environment, or
provider webhook secrets.

## `local_webhooks` Primitive

`local_webhooks` is a provider-neutral Rust/Lua primitive. It owns a
127.0.0.1-only listener and dispatches requests to plugin workers through
bounded mailboxes. It never binds `0.0.0.0`.

API shape:

```lua
local_webhooks.register({
  id = "github.repo-webhook",
  methods = { "POST" },
  path = "/webhooks/<claim_id>/<route_token>",
  body_limit = 1024 * 1024,
  timeout_ms = 10000,
  response_mode = "handler",
}, function(request)
  return {
    status = 202,
    headers = { ["content-type"] = "application/json" },
    body = json.encode({ ok = true }),
  }
end)

local_webhooks.unregister("github.repo-webhook")
```

Route tokens must be unguessable. Use at least 128 bits of entropy, reject
collisions during registration, and never accept a caller-supplied token that
already exists for another route. The listener should accept `POST` and `PUT`
only when registered. Other methods return `405`.

Allowed request bodies are bounded byte buffers. The listener must return `413`
when `body_limit` would be exceeded and must not continue reading unbounded
input. `Content-Length` and `Transfer-Encoding: chunked` are both allowed only
while enforcing the same accumulated limit; unsupported transfer codings are
rejected before plugin dispatch.

`local_webhooks` intentionally does not default-deny by content type, because
provider webhook content types vary across JSON, form-encoded, XML, and opaque
byte payloads. It preserves `content-type` verbatim and passes missing or
unexpected values through unless the registered route declares an explicit
allowlist. Provider plugins must validate and reject unexpected content types
before parsing provider-specific payloads.

Local listener port allocation must avoid collisions between `~/.botster` and
`~/.botster-dev` hubs on the same device. The chosen port is part of connector
state and the cloudflared ingress service URL; it is not hardcoded.

## Request And Response Shape

Plugin workers receive a plain request table:

```lua
{
  request_id = "wh_...",
  route_id = "github.repo-webhook",
  method = "POST",
  path = "/webhooks/surl_123/<route_token>",
  query = "delivery=1",
  headers = { ["x-github-event"] = "push" },
  body = "...",
  body_truncated = false,
  remote_addr = "127.0.0.1",
  received_at = "2026-05-28T00:00:00Z"
}
```

Cloudflared-originated requests arrive at the local listener from
`127.0.0.1`. `remote_addr` records that socket peer only. Forwarded client
identity headers such as `CF-Connecting-IP`, `X-Forwarded-For`, and
`X-Forwarded-Proto` stay in `headers` for provider plugins to interpret or
ignore; `local_webhooks` must not rewrite them into trusted client identity.

Provider plugins own signature verification, provider challenge parsing,
idempotency keys, replay handling, event normalization, and durable processing.
`local_webhooks` only enforces transport limits and dispatch semantics.

Response modes:

| Mode | Behavior |
|---|---|
| `static` | Return a configured status/body without waiting for plugin work. |
| `ack` | Return `202` after the request is accepted into a plugin mailbox. |
| `handler` | Wait for the plugin worker response until `timeout_ms`. |

Handler responses include `status`, `headers`, and `body`. Missing handler
responses or timeouts return `504`. Full mailboxes return `503` or `429` with a
bounded body. Plugin handler failures return `500` without leaking stack traces
to the caller.

## Reload And Reconciliation

Each registered route carries `owner_plugin`, `handler_ref`, and
`plugin_generation`. The hub registry may store descriptors and handler refs,
but not `mlua::Function` closures. Execution must happen in the plugin worker.

On plugin reload or unload, `local_webhooks` removes routes for the old
generation. URL release, token rotation, and Rails-side hostname revocation
disable the affected route generation before connector changes are applied.
In-flight requests may finish only if their generation is still current. Stale
completions are ignored and cannot write late responses after a route is
replaced, released, or revoked. New requests for a released claim return `410`;
new requests for an unknown or Rails-revoked hostname/route return `404`.

Token rotation must prefer a drain-and-reload sequence: mint and store the new
token generation, start or reload the connector, wait for health, then revoke
the old generation. If the connector cannot drain cleanly before the timeout,
the stable-url plugin may reset the local connector process and must mark
affected claims `reconciling` or `unhealthy` until the new connector is healthy.
Rails hostname revocation is stricter: disable the local route immediately,
stop accepting new requests for that hostname, publish entity state, and let
any old in-flight completions fall under the stale-generation rule.

The stable-url plugin reconciliation loop compares Rails desired state,
plugin.db claims, encrypted secret presence, generated cloudflared config,
process health, and published entities. It repairs missing config, restarts
stale connectors, marks orphaned claims unhealthy, and releases hostnames only
through the claim/release contract or Rails revocation.

## Ticket Decomposition

| Slice | Boundary | Required verification |
|---|---|---|
| Rails broker | Routes, models, auth, Cloudflare adapter PORO | Model/controller/adapter tests; token redaction assertions; rotation/revocation tests. |
| `local_webhooks` primitive | Rust listener plus Lua API | CLI unit/integration tests through `cd cli && ./test.sh`; body limit, timeout, mailbox-full, generation fencing, and method rejection tests. |
| Cloudflare stable-url plugin | Device plugin with plugin.db, secrets, entities, connector process | Lua/plugin tests for claim/release/list, entity publication, secret pointer storage, connector config generation, and reconciliation. |
| Consumer plugins | Provider verification and event policy | Provider-specific signature/challenge/replay tests; no access to Cloudflare tunnel token. |
| Static guardrails | Source/documentation assertions | No hub-stored `mlua::Function` route handlers; no real-looking secrets in examples; no Cloudflare account credentials outside Rails broker code. |

Additional negative tests:

- Simulated slow body reads and handler latency must not block the hub event
  loop or WebRTC/session queues.
- Hot reload must re-register a route against a running hub without nested
  `block_on` panics.
- Reviewer checks every acceptance checklist row below resolves to a section in
  this spec.

## Acceptance Checklist

| Requirement | Covered by |
|---|---|
| Rails broker API | Rails Broker API |
| Cloudflare tunnel/hostname lifecycle | Cloudflare Lifecycle |
| Hub token delivery semantics | Hub Token Delivery And Storage |
| Stable URL claim model | Stable URL Claim Model |
| Local webhook listener API | `local_webhooks` Primitive |
| Request/response shape | Request And Response Shape |
| Body limits | `local_webhooks` Primitive |
| Timeout behavior | Request And Response Shape |
| Reconciliation rules | Reload And Reconciliation |
| Security decisions | Hub Token Delivery And Storage, Stable URL Entity Contract |
| Rotation/revocation | Hub Token Delivery And Storage |
| One active owner rule | Stable URL Claim Model |
| Ticket decomposition against Botster boundaries | Ticket Decomposition |
| No real-looking secrets or Cloudflare account credentials in examples | This document uses placeholder hostnames, omits all token values, and the Stable URL Entity Contract forbids publishing token secret pointers, raw token bytes, account credentials, raw `config.yml`, cloudflared environment, or provider webhook secrets. |

## References

Vault constraints loaded for this contract:

- `plugin stable webhook urls need a generic ingress contract`: stable
  hostnames are transport, not provider webhook policy.
- `stable url claims should be a shared plugin resource`: claim/release/list
  belongs to the stable-url plugin, not every consumer plugin.
- `rails serves webhooks auth and relay not business logic`: Rails brokers
  auth and relay/lifecycle; edge behavior stays in CLI/plugins.
- `botster core lua owns plugin framework primitives not product policy`:
  `local_webhooks` is generic, Cloudflare and provider policy are plugins.
- `botster plugin runtime uses supervisor plus per plugin workers`: webhook
  handlers execute through plugin workers.
- `botster data plane bypasses the hub through session and client actors`:
  inbound body handling and slow clients must not burden hub hot paths.
- `lua primitives expose dual calling conventions to control blocking
  semantics`: callback/table-first async paths are the safe runtime default.
- `plugin bootstrap must not call sync block_on primitives after hub event loop
  starts`: reload tests must catch nested-runtime panics.
- `cloudflared named tunnels route multiple hostnames via config.yml ingress
  array`: one named tunnel routes many stable hostnames.
- `cloudflared ingress service ports must match foreman's actual assigned
  port`: connector config uses the runtime listener port, not guessed env.
- `botster plugin entities are canonical for plugin-owned dynamic state`:
  stable URL state publishes through entity frames.
