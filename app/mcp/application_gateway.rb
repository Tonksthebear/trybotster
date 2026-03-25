# frozen_string_literal: true

class ApplicationGateway < ActionMCP::Gateway
  # Authenticate MCP connections via HubToken (Bearer token in Authorization header).
  # This matches the authentication pattern used by the rest of the API.
  identified_by HubTokenIdentifier
end
