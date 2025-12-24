# frozen_string_literal: true

module TunnelProxy
  extend ActiveSupport::Concern

  private

  def proxy_to_tunnel(hub_agent)
    request_id = SecureRandom.uuid
    hub = hub_agent.hub

    # Stream name format: tunnel_hub_{user_id}_{hub_identifier}
    stream_name = "tunnel_hub_#{hub.user_id}_#{hub.identifier}"

    ActionCable.server.broadcast(stream_name, {
      type: "http_request",
      request_id: request_id,
      agent_session_key: hub_agent.session_key,
      method: request.method,
      path: "/#{params[:path]}",
      query_string: request.query_string,
      headers: proxy_headers(hub_agent),
      body: request.body&.read || ""
    })

    response_data = TunnelResponseStore.wait_for(request_id, timeout: 30)

    if response_data
      hub_agent.update(tunnel_last_request_at: Time.current)
      render_proxied_response(response_data)
    else
      render plain: "Tunnel timeout - local server not responding", status: :gateway_timeout
    end
  end

  def render_not_found(message)
    render plain: message, status: :bad_gateway
  end

  def proxy_headers(hub_agent)
    headers = {}
    request.headers.each do |key, value|
      next unless key.start_with?("HTTP_")
      next if %w[HTTP_HOST HTTP_CONNECTION].include?(key)

      header_name = key.sub("HTTP_", "").tr("_", "-")
      headers[header_name] = value
    end
    headers["X-Forwarded-Host"] = request.host
    headers["X-Forwarded-Proto"] = request.protocol.chomp("://")
    headers["X-Botster-Agent"] = hub_agent.session_key
    headers
  end

  def render_proxied_response(data)
    data["headers"]&.each do |key, value|
      next if %w[transfer-encoding connection].include?(key.downcase)

      response.headers[key] = value
    end

    render body: data["body"], status: data["status"], content_type: data["content_type"]
  end
end
