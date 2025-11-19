# frozen_string_literal: true

class ApplicationMCPTool < ActionMCP::Tool
  abstract!

  # Helper method to get session client info from ActionMCP execution context
  def session_client_info
    if respond_to?(:execution_context) && execution_context.is_a?(Hash)
      session = execution_context[:session]
      if session.respond_to?(:client_info) && session.client_info.present?
        return session.client_info
      end
    end

    nil
  rescue => e
    Rails.logger.debug "Could not access session client info: #{e.message}"
    nil
  end

  # Helper method to access request metadata from ActionMCP execution context
  def request_metadata
    if respond_to?(:execution_context) && execution_context.is_a?(Hash)
      meta = execution_context.dig(:request, :params, :_meta)
      return meta if meta.present?
    end

    {}
  rescue => e
    Rails.logger.debug "Could not access request metadata: #{e.message}"
    {}
  end

  # Helper to detect the client type and version
  def detect_client_type
    # Try session client_info first (most reliable)
    client_info = session_client_info
    if client_info.is_a?(Hash) && client_info["name"].present?
      name = client_info["name"].downcase
      version = client_info["version"]

      if name.include?("visual studio code") || name.include?("vscode")
        return version ? "VS Code (#{version})" : "VS Code"
      elsif name.include?("cursor")
        return version ? "Cursor (#{version})" : "Cursor"
      elsif name.include?("claude")
        return version ? "Claude Desktop (#{version})" : "Claude Desktop"
      else
        # Generic client with version if available
        return version ? "#{client_info["name"]} (#{version})" : client_info["name"]
      end
    end

    # Fallback to metadata-based detection
    meta = request_metadata
    if meta.is_a?(Hash)
      return "VS Code" if meta["vscode.conversationId"]
      return "Cursor" if meta["cursor.conversationId"]
      return "Claude Desktop" if meta.keys.any? { |k| k.to_s.include?("claude") }
    end

    "MCP Client"
  end

  # Helper to get user attribution string
  def user_attribution
    current_user&.email || current_user&.username || "MCP User"
  end

  # Helper to generate attribution footer for GitHub content
  def attribution_footer
    client_info = detect_client_type
    user_info = user_attribution
    "\n\n---\n_Created via #{client_info} by #{user_info} using trybotster MCP tools_"
  end
end
