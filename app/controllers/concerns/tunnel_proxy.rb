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

    body = data["body"]
    content_type = data["content_type"]

    # Inject <base> tag for HTML responses so relative/absolute URLs resolve through the proxy
    if content_type&.include?("text/html") && body.present?
      body = inject_base_tag(body)
    end

    render body: body, status: data["status"], content_type: content_type
  end

  # Rewrite URLs in HTML to route through the proxy
  # This handles absolute URLs like /assets/foo.css which <base> doesn't fix
  def inject_base_tag(html)
    base_url = proxy_base_url.chomp("/") # Remove trailing slash for clean concatenation

    # Rewrite absolute URLs in common HTML attributes
    # Matches: href="/...", src="/...", action="/...", data-src="/..."
    # Handles both single and double quotes
    result = html.gsub(/(\s(?:href|src|action|data-src)\s*=\s*)(["'])\/(?!\/)/i) do |_match|
      attr_prefix = $1
      quote = $2
      "#{attr_prefix}#{quote}#{base_url}/"
    end

    # Also rewrite url() in inline styles: url(/assets/...)
    result = result.gsub(/url\(\s*(["']?)\/(?!\/)/) do |_match|
      quote = $1
      "url(#{quote}#{base_url}/"
    end

    # Inject <base> tag for relative URLs as well
    base_tag = %(<base href="#{base_url}/">)

    if result.include?("<head>")
      result.sub("<head>", "<head>\n#{base_tag}")
    elsif result.include?("<HEAD>")
      result.sub("<HEAD>", "<HEAD>\n#{base_tag}")
    elsif result.include?("<html>") || result.include?("<HTML>")
      result.sub(/<html>/i, "\\0\n<head>#{base_tag}</head>")
    else
      "#{base_tag}\n#{result}"
    end
  end

  # Build the base URL for the proxy (e.g., /preview/hub-id/agent-id/)
  def proxy_base_url
    raise NotImplementedError, "Subclass must implement proxy_base_url"
  end
end
