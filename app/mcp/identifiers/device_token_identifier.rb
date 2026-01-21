# frozen_string_literal: true

# MCP gateway identifier that authenticates via MCPToken (btmcp_).
#
# Agents use MCP tokens for scoped access to MCP operations only.
# Tokens are passed as Bearer tokens in the Authorization header.
class DeviceTokenIdentifier < ActionMCP::GatewayIdentifier
  identifier :user
  authenticates :api_key

  def resolve
    token = extract_bearer_token
    return nil if token.blank?

    mcp_token = MCPToken.find_by(token: token)
    return nil unless mcp_token

    mcp_token.touch_usage!(ip: request_ip)
    mcp_token.user
  end

  private

  def request_ip
    @request.remote_ip || "unknown"
  end
end
