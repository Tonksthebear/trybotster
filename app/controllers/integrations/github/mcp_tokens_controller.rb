# frozen_string_literal: true

module Integrations
  module Github
    class McpTokensController < ApplicationController
      include ApiKeyAuthenticatable

      skip_before_action :verify_authenticity_token
      before_action :authenticate_device!

      # POST /integrations/github/mcp_tokens
      #
      # Creates or returns the MCP token for the authenticated device.
      # Called by plugins that need to pass scoped credentials to agents.
      #
      # Auth: Bearer btstr_... (device token)
      # Response: { "token": "btmcp_...", "mcp_url": "https://mcp.trybotster.com" }
      def create
        mcp_token = @device.mcp_token || @device.create_mcp_token!(name: "#{@device.name} MCP")
        mcp_token.touch_usage!(ip: request.remote_ip)

        render json: {
          token: mcp_token.token,
          mcp_url: mcp_server_url
        }
      end

      private

      def authenticate_device!
        token = extract_api_key
        if token.blank?
          return render_unauthorized("API key required")
        end

        device_token = DeviceToken.find_by(token: token)
        unless device_token&.device
          return render_unauthorized("Invalid API key")
        end

        device_token.touch_usage!(ip: request.remote_ip)
        @device = device_token.device
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
