# frozen_string_literal: true

module Integrations
  module Github
    # MCP token for agent authentication with the MCP server.
    #
    # Scoped to MCP operations only - agents use this token to authenticate
    # tool calls without having access to the full hub API token.
    class MCPToken < ApplicationRecord
      TOKEN_PREFIX = "btmcp_"
      TOKEN_LENGTH = 32

      belongs_to :device

      encrypts :token, deterministic: true

      validates :token, presence: true, uniqueness: true

      before_validation :generate_token, on: :create

      def touch_usage!(ip: nil)
        update_columns(last_used_at: Time.current, last_ip: ip)
      end

      def display_token
        "#{TOKEN_PREFIX}...#{token.last(8)}"
      end

      # Convenience method to get the user through device
      def user
        device&.user
      end

      private

      def generate_token
        self.token ||= "#{TOKEN_PREFIX}#{SecureRandom.urlsafe_base64(TOKEN_LENGTH)}"
      end
    end
  end
end
