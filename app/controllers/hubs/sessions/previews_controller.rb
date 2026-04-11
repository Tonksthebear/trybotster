# frozen_string_literal: true

module Hubs
  module Sessions
    # Serves the preview bootstrap page and service worker for session port forwarding.
    #
    # URL: /hubs/:hub_id/sessions/:session_uuid/preview/*path
    #
    # The service worker handles all actual proxying over the E2E WebSocket connection.
    # This controller only serves:
    # 1. Bootstrap page - registers service worker, then reloads
    # 2. Preview shell - establishes WebSocket, relays requests from SW
    # 3. Service worker JS - intercepts requests and proxies via WebSocket
    #
    # No backend proxying - everything goes through the client-side E2E channel.
    class PreviewsController < ApplicationController
      skip_forgery_protection only: [ :bootstrap, :shell, :service_worker ]

      before_action :authenticate_user!, unless: :public_preview_session?
      before_action :set_hub
      before_action :set_session_uuid

      # GET /hubs/:hub_id/sessions/:session_uuid/preview
      def bootstrap
        render_bootstrap
      end

      # GET /hubs/:hub_id/sessions/:session_uuid/preview/shell
      def shell
        render_preview_shell
      end

      # GET /hubs/:hub_id/sessions/:session_uuid/preview/sw.js
      def service_worker
        set_public_preview_headers if @public_preview
        @proxy_base = scope_path
        response.headers["Content-Type"] = "application/javascript"
        response.headers["Service-Worker-Allowed"] = scope_path
        render template: "hubs/sessions/previews/service_worker", formats: [ :js ], layout: false
      end

      private

      def set_hub
        if @public_preview
          # Already looked up in public_preview_session? — reuse cached hub
          Current.hub = @_preview_hub
        else
          Current.hub = current_user.hubs.find_by(id: params[:hub_id])
        end

        unless Current.hub
          if @public_preview
            render plain: "Preview not available", status: :not_found
          else
            redirect_to hubs_path, alert: "Hub not found"
          end
        end
      end

      def set_session_uuid
        Current.session_uuid = params[:session_uuid]
      end

      def public_preview_session?
        if current_user&.hubs&.exists?(id: params[:hub_id])
          @public_preview = false
          return false
        end

        @_preview_hub = Hub.find_by(id: params[:hub_id])
        if @_preview_hub&.alive? && @_preview_hub&.public_preview_enabled?(params[:session_uuid])
          @public_preview = true
          @public_preview_port = @_preview_hub.public_preview_port(params[:session_uuid])
        else
          @public_preview = false
        end
      end

      def sw_version
        @sw_version ||= Digest::MD5.hexdigest(
          File.read(Rails.root.join("app/views/hubs/sessions/previews/service_worker.js.erb"))
        )[0..7]
      end

      def render_bootstrap
        set_public_preview_headers if @public_preview
        @sw_path = hub_session_preview_service_worker_path(Current.hub, Current.session_uuid)
        @scope = scope_path
        @preview_port = preview_port
        @initial_url = initial_preview_path
        @shell_path = "#{scope_path}/shell#{preview_query}"
        @sw_version = sw_version
        render template: "hubs/sessions/previews/bootstrap", layout: false
      end

      def render_preview_shell
        set_public_preview_headers if @public_preview
        @scope = scope_path
        @preview_port = preview_port
        @initial_url = initial_preview_path
        render template: "hubs/sessions/previews/shell", layout: false
      end

      def set_public_preview_headers
        response.headers["Cache-Control"] = "no-store"
        response.headers["X-Robots-Tag"] = "noindex, nofollow"
      end

      def scope_path
        "/hubs/#{Current.hub.id}/sessions/#{Current.session_uuid}/preview"
      end

      def preview_port
        @preview_port ||= begin
          raw_port = if @public_preview
            @public_preview_port
          else
            params[:port].presence || Current.hub&.public_preview_port(Current.session_uuid)
          end

          port = Integer(raw_port, exception: false)
          port if port&.positive?
        end
      end

      def preview_query
        query = {}
        query[:port] = preview_port if preview_port
        query[:path] = initial_preview_path unless initial_preview_path == "/"
        return "" if query.empty?

        "?#{query.to_query}"
      end

      def initial_preview_path
        @initial_preview_path ||= begin
          path = params[:path].to_s.strip
          if path.blank? || path.start_with?("http://", "https://", "//")
            "/"
          else
            path = "/#{path}" unless path.start_with?("/")
            path
          end
        end
      end
    end
  end
end
