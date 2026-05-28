# Cloudflare Stable URLs

Device-scoped stable webhook URL pool backed by `plugin.db`.

Public Lua API:

```lua
local stable_urls = require("lib.stable_urls")

local row = stable_urls.claim({
  id = "surl_seed_1", -- optional; omit to claim the first available URL
  owner_plugin = "github",
  owner_key = "repo:owner/name",
  purpose = "webhook",
  local_service_url = "http://127.0.0.1:47123",
})

stable_urls.release({
  id = row.id,
  owner_plugin = "github",
  owner_key = "repo:owner/name",
})
```

Session-facing MCP tools mirror the Lua API:

- `stable_urls_claim`
- `stable_urls_release`
- `stable_urls_list`
- `stable_urls_get`

Owner identity is `owner_plugin + owner_key`. `owner_id` is intentionally not a
public field. This slice only creates `available` and `claimed` transitions;
`reconciling`, `unhealthy`, and `revoked` are reserved statuses for later
connector reconciliation work.

The plugin publishes `cloudflare-stable-urls.stable_url` entity records with
non-secret fields only. It never returns Cloudflare connector tokens,
`token_secret_key`, account credentials, raw cloudflared config, cloudflared
environment, provider webhook secrets, or audit metadata through Lua API, MCP,
or entity frames.
