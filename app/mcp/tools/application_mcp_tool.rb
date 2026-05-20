# frozen_string_literal: true

class ApplicationMCPTool < ActionMCP::Tool
  abstract!

  validate :default_repo_from_botster_context

  # Extract idempotency key from request metadata.
  # MCP callers can send this via _meta.idempotencyKey in the request.
  def idempotency_key_from_request
    meta = request_metadata
    return nil unless meta.is_a?(Hash)

    hash_value(meta, "idempotencyKey") || hash_value(meta, "idempotency_key")
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

  # Helper method to access request metadata from ActionMCP execution context
  def request_metadata
    if respond_to?(:execution_context) && execution_context.is_a?(Hash)
      request = hash_value(execution_context, "request")
      params = hash_value(request, "params") if request.is_a?(Hash)
      meta = hash_value(params, "_meta") if params.is_a?(Hash)
      return meta if meta.present?
    end

    {}
  rescue => e
    Rails.logger.debug "Could not access request metadata: #{e.message}"
    {}
  end

  # Helper to identify the Botster agent/session invoking the tool.
  def botster_session_attribution
    botster_agent_attribution || "Botster MCP session"
  end

  def botster_agent_attribution
    botster = botster_agent_metadata
    return nil unless botster.is_a?(Hash)

    agent_name = hash_value(botster, "agent_name") || hash_value(botster, "session_name")
    branch_name = hash_value(botster, "branch_name")
    session_uuid = hash_value(botster, "session_uuid")

    label = agent_name.presence || "agent"
    details = []
    details << branch_name if branch_name.present?
    details << session_uuid.to_s.first(12) if session_uuid.present?

    if details.any?
      "Botster #{label} (#{details.join(", ")})"
    else
      "Botster #{label}"
    end
  end

  def botster_agent_metadata
    meta = request_metadata
    hash_value(meta, "botster") if meta.is_a?(Hash)
  end

  def botster_agent_repo
    botster = botster_agent_metadata
    return nil unless botster.is_a?(Hash)

    hash_value(botster, "repo").presence
  end

  # Helper to get user attribution string
  def user_attribution
    current_user&.email || current_user&.username || "MCP User"
  end

  # Helper to generate attribution footer for GitHub content
  def attribution_footer
    agent_info = botster_session_attribution
    user_info = user_attribution
    "\n\n---\n_Created by #{agent_info} for #{user_info} using trybotster MCP tools_"
  end

  private

  def default_repo_from_botster_context
    return unless self.class._schema_properties.key?("repo")
    return unless respond_to?(:repo) && respond_to?(:repo=)
    return if repo.present?

    self.repo = botster_agent_repo
  end

  def hash_value(hash, key)
    return nil unless hash.is_a?(Hash)

    hash[key] || hash[key.to_sym]
  end

  def normalize_github_markdown_body(value)
    text = value.to_s
    return text unless text.include?("\\n") || text.include?("\\r\\n")
    return text if text.include?("\n") || text.include?("\r")

    text.gsub("\\r\\n", "\n").gsub("\\n", "\n")
  end
end
