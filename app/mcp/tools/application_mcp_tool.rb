# frozen_string_literal: true

class ApplicationMCPTool < ActionMCP::Tool
  abstract!

  # Extract idempotency key from request metadata
  # Clients can send this via _meta.idempotencyKey in the request
  def idempotency_key_from_request
    meta = request_metadata
    return nil unless meta.is_a?(Hash)

    meta["idempotencyKey"] || meta["idempotency_key"] || meta[:idempotencyKey] || meta[:idempotency_key]
  end

  # Check if we have a cached response for this idempotency key
  # Returns the cached response if found, nil otherwise
  def check_idempotency_cache
    key = idempotency_key_from_request
    return nil unless key.present?

    idempotency_record = IdempotencyKey.find_by(key: key)
    return nil unless idempotency_record&.completed?

    # Return cached response
    Rails.logger.info "Returning cached response for idempotency key: #{key}"
    JSON.parse(idempotency_record.response_body)
  rescue JSON::ParserError => e
    Rails.logger.warn "Failed to parse cached response for idempotency key #{key}: #{e.message}"
    nil
  end

  # Store the response for this idempotency key
  def store_idempotency_response(response_text, status: 200)
    key = idempotency_key_from_request
    return unless key.present?

    idempotency_record = IdempotencyKey.find_or_create_for_request(
      key,
      self.class.tool_name,
      tool_params
    )

    return if idempotency_record.completed?

    idempotency_record.mark_completed!(
      status: status,
      body: { success: true, text: response_text }.to_json
    )
    Rails.logger.info "Stored response for idempotency key: #{key}"
  rescue => e
    Rails.logger.warn "Failed to store idempotency response for key #{key}: #{e.message}"
  end

  # Get tool parameters as a hash (for idempotency key storage)
  def tool_params
    self.class._schema_properties.keys.each_with_object({}) do |prop, hash|
      hash[prop] = send(prop) if respond_to?(prop)
    end
  end

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
