# frozen_string_literal: true

module Hubs
  module Agents
    # Serves the preview bootstrap page and service worker for PTY port forwarding.
    #
    # URL: /hubs/:hub_id/agents/:index/:pty_index/preview/*path
    #
    # The service worker handles all actual proxying over the E2E WebSocket connection.
    # This controller only serves:
    # 1. Bootstrap page - registers service worker, then reloads
    # 2. Preview shell - establishes WebSocket, relays requests from SW
    # 3. Service worker JS - intercepts requests and proxies via WebSocket
    #
    # No backend proxying - everything goes through the client-side E2E channel.
    #
    # == CLI Interface for HTTP Proxy ==
    #
    # The CLI must handle HTTP proxy messages on the TerminalRelayChannel for
    # PTY streams that have port forwarding enabled (typically pty_index=1).
    #
    # === Request Message (Browser -> CLI) ===
    #
    #   {
    #     "type": "http_request",
    #     "request_id": "req_123_1234567890",  // Unique ID for correlating response
    #     "method": "GET",                      // HTTP method
    #     "path": "/api/users?page=1",          // Path with query string
    #     "headers": {                          // HTTP headers (host/connection excluded)
    #       "Accept": "application/json",
    #       "Content-Type": "application/json"
    #     },
    #     "body": "..."                         // Request body (string, may be null)
    #   }
    #
    # === Response Message (CLI -> Browser) ===
    #
    #   {
    #     "type": "http_response",
    #     "request_id": "req_123_1234567890",  // Must match request
    #     "status": 200,                        // HTTP status code
    #     "status_text": "OK",                  // HTTP status text
    #     "headers": {                          // Response headers
    #       "Content-Type": "text/html",
    #       "Cache-Control": "no-cache"
    #     },
    #     "body": "base64encodedcontent..."     // Response body, BASE64 ENCODED
    #   }
    #
    # === Implementation Notes ===
    #
    # 1. The CLI should proxy the request to localhost:{forwarded_port}
    # 2. Response body MUST be base64 encoded (binary-safe transport)
    # 3. request_id must be echoed back exactly for correlation
    # 4. Timeout: Browser waits 30 seconds for response
    # 5. Messages flow over the same E2E encrypted channel as terminal I/O
    #
    class PreviewsController < ApplicationController
      skip_forgery_protection only: [ :show, :service_worker ]

      before_action :authenticate_user!
      before_action :set_hub
      before_action :set_indices

      # GET /hubs/:hub_id/agents/:index/:pty_index/preview
      # GET /hubs/:hub_id/agents/:index/:pty_index/preview/*path
      #
      # First request: serves bootstrap page to register service worker
      # Second request (after SW registered): serves preview shell with WebSocket connection
      # Subsequent requests: service worker intercepts and proxies via WebSocket
      def show
        if service_worker_ready?
          # SW is registered - serve the preview shell page
          # This page establishes WebSocket and relays requests from SW
          render_preview_shell
        else
          # No SW yet - serve bootstrap to register it
          render_bootstrap
        end
      end

      # GET /hubs/:hub_id/agents/:index/:pty_index/preview/sw.js
      def service_worker
        @proxy_base = scope_path
        response.headers["Content-Type"] = "application/javascript"
        response.headers["Service-Worker-Allowed"] = scope_path
        render template: "hubs/agents/previews/service_worker", formats: [ :js ], layout: false
      end

      private

      def set_hub
        Current.hub = current_user.hubs.find_by(id: params[:hub_id])
        redirect_to hubs_path, alert: "Hub not found" unless Current.hub
      end

      def set_indices
        @agent_index = params[:index].to_i
        @pty_index = params[:pty_index].to_i
      end

      def service_worker_ready?
        cookies[:preview_sw] == sw_version
      end

      def sw_version
        @sw_version ||= Digest::MD5.hexdigest(
          File.read(Rails.root.join("app/views/hubs/agents/previews/service_worker.js.erb"))
        )[0..7]
      end

      def render_bootstrap
        @sw_path = hub_agent_pty_service_worker_path(Current.hub, @agent_index, @pty_index)
        @scope = scope_path
        @sw_version = sw_version
        @hub_id = Current.hub.id
        @agent_index_value = @agent_index
        @pty_index_value = @pty_index
        render template: "hubs/agents/previews/bootstrap", layout: false
      end

      def render_preview_shell
        @hub_id = Current.hub.id
        @agent_index_value = @agent_index
        @pty_index_value = @pty_index
        @scope = scope_path
        render template: "hubs/agents/previews/shell", layout: false
      end

      def scope_path
        "/hubs/#{Current.hub.id}/agents/#{@agent_index}/#{@pty_index}/preview"
      end
    end
  end
end
