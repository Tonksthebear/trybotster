-- @template Cloudflare Stable URLs
-- @description Stable webhook URL pool and claim API backed by plugin.db
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/cloudflare_stable_urls/db.lua
-- @scope device
-- @version 0.1.0

local db = plugin.db{
    version = 1,
    models = {
        stable_urls = {
            id = { "text", required = true, primary = true },
            hostname = { "text", required = true, unique = true },
            public_url = { "text", required = true },
            status = { "text", required = true },
            token_secret_key = { "text" },
            token_version = { "text" },
            provider_metadata_json = { "text" },
            last_checked_at = { "integer" },
            message = { "text" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        claims = {
            id = { "text", required = true, primary = true },
            stable_url_id = { "text", required = true },
            owner_plugin = { "text", required = true },
            owner_key = { "text", required = true },
            purpose = { "text", required = true },
            session_uuid = { "text" },
            local_service_url = { "text" },
            local_route = { "text" },
            local_port = { "integer" },
            created_at = { "integer", required = true },
            released_at = { "integer" },
            release_reason = { "text" },
        },
        audit_events = {
            id = { "text", required = true, primary = true },
            stable_url_id = { "text" },
            claim_id = { "text" },
            action = { "text", required = true },
            owner_plugin = { "text" },
            owner_key = { "text" },
            session_uuid = { "text" },
            message = { "text" },
            metadata_json = { "text" },
            created_at = { "integer", required = true },
        },
    },
}

local indexes = {
    "CREATE INDEX IF NOT EXISTS idx_cloudflare_stable_urls_status ON stable_urls(status, updated_at)",
    "CREATE INDEX IF NOT EXISTS idx_cloudflare_stable_urls_claims_owner ON claims(owner_plugin, owner_key, released_at)",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_cloudflare_stable_urls_active_claim ON claims(stable_url_id) WHERE released_at IS NULL",
    "CREATE INDEX IF NOT EXISTS idx_cloudflare_stable_urls_audit_url_created ON audit_events(stable_url_id, created_at)",
}

for _, statement in ipairs(indexes) do
    local ok, err = pcall(function()
        db:eval(statement)
    end)
    if not ok then
        log.warn("[cloudflare-stable-urls] Failed to create index: " .. tostring(err))
    end
end

return db
