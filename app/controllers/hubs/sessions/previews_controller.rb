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

      before_action :authenticate_user!
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
        @proxy_base = scope_path
        response.headers["Content-Type"] = "application/javascript"
        response.headers["Service-Worker-Allowed"] = scope_path
        render template: "hubs/sessions/previews/service_worker", formats: [ :js ], layout: false
      end

      private

      def set_hub
        Current.hub = current_user.hubs.find_by(id: params[:hub_id])

        unless Current.hub
          redirect_to hubs_path, alert: "Hub not found"
        end
      end

      def set_session_uuid
        Current.session_uuid = params[:session_uuid]
      end

      def sw_version
        @sw_version ||= Digest::MD5.hexdigest(
          File.read(Rails.root.join("app/views/hubs/sessions/previews/service_worker.js.erb"))
        )[0..7]
      end

      def render_bootstrap
        @sw_path = hub_session_preview_service_worker_path(Current.hub, Current.session_uuid)
        @scope = scope_path
        @preview_port = preview_port
        @initial_url = initial_preview_path
        @shell_path = "#{scope_path}/shell#{preview_query}"
        @sw_version = sw_version
        render template: "hubs/sessions/previews/bootstrap", layout: false
      end

      def render_preview_shell
        @scope = scope_path
        @preview_port = preview_port
        @initial_url = initial_preview_path
        render template: "hubs/sessions/previews/shell", layout: false
      end

      def scope_path
        "/hubs/#{Current.hub.id}/sessions/#{Current.session_uuid}/preview"
      end

      def preview_port
        @preview_port ||= begin
          port = Integer(params[:port].presence, exception: false)
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
