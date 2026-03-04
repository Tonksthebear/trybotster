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
        redirect_to hubs_path, alert: "Hub not found" unless Current.hub
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
        @shell_path = "#{scope_path}/shell"
        @sw_version = sw_version
        render template: "hubs/sessions/previews/bootstrap", layout: false
      end

      def render_preview_shell
        @scope = scope_path
        render template: "hubs/sessions/previews/shell", layout: false
      end

      def scope_path
        "/hubs/#{Current.hub.id}/sessions/#{Current.session_uuid}/preview"
      end
    end
  end
end
