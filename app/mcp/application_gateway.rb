# frozen_string_literal: true

class ApplicationGateway < ActionMCP::Gateway
  # Authenticate MCP connections via DeviceToken (Bearer token in Authorization header).
  # This matches the authentication pattern used by the rest of the API.
  identified_by DeviceTokenIdentifier
end
