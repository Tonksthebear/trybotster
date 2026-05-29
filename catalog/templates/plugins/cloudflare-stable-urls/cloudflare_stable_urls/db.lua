-- @template Cloudflare Stable URLs
-- @description Durable state for hub-level Cloudflare stable URL claims and connector status
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/cloudflare_stable_urls/db.lua
-- @scope device
-- @version 1.0.0

return plugin.db{
    version = 1,
    models = {
        connector_state = {
            id = { "text", required = true, primary = true },
            cloudflare_tunnel_id = { "text" },
            cloudflare_tunnel_name = { "text" },
            token_version = { "integer" },
            token_secret_key = { "text" },
            token_path = { "text" },
            config_path = { "text" },
            connector_session_uuid = { "text" },
            connector_generation = { "integer" },
            status = { "text", required = true },
            message = { "text" },
            retry_count = { "integer" },
            updated_at = { "integer", required = true },
        },
        stable_url_claims = {
            id = { "text", required = true, primary = true },
            hostname = { "text", required = true },
            public_url = { "text", required = true },
            owner_plugin = { "text" },
            owner_key = { "text" },
            purpose = { "text" },
            local_service_url = { "text", required = true },
            status = { "text", required = true },
            message = { "text" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
    },
}
