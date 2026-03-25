# frozen_string_literal: true

module Integrations
  module Github
    class MCPTokensController < ApplicationController
      include ApiKeyAuthenticatable

      skip_before_action :verify_authenticity_token
      before_action :authenticate_hub!

      # POST /integrations/github/mcp_tokens
      #
      # Creates or returns the MCP token for the authenticated hub.
      # Called by plugins that need to pass scoped credentials to agents.
      #
      # Auth: Bearer btstr_... (hub token)
      # Response: { "token": "btmcp_...", "mcp_url": "https://mcp.trybotster.com" }
      def create
        mcp_token = @hub.mcp_token || @hub.create_mcp_token!(name: "#{@hub.name} MCP")
        mcp_token.touch_usage!(ip: request.remote_ip)

        render json: {
          token: mcp_token.token,
          mcp_url: mcp_server_url
        }
      end

      private

      def authenticate_hub!
        token = extract_api_key
        if token.blank?
          return render_unauthorized("API key required")
        end

        hub_token = HubToken.find_by(token: token)
        unless hub_token&.hub
          return render_unauthorized("Invalid API key")
        end

        hub_token.touch_usage!(ip: request.remote_ip)
        @hub = hub_token.hub
      end

      def mcp_server_url
        if Rails.env.development?
          "https://mcp-dev.trybotster.com"
        else
          "https://mcp.trybotster.com"
        end
      end
    end
  end
end
