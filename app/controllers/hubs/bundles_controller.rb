# frozen_string_literal: true

module Hubs
  # Returns Signal Protocol PreKeyBundle for browser session establishment.
  #
  # Primary flow is QR code scanning (bundle embedded in URL fragment).
  # This endpoint is a fallback for session recovery when fragment is missing.
  class BundlesController < ApplicationController
    before_action :authenticate_user!
    before_action :set_hub

    # GET /hubs/:hub_identifier/bundle
    def show
      # For now, bundles are only available via QR code URL fragment.
      # The CLI generates a fresh PreKeyBundle and embeds it in the QR code.
      #
      # Future: CLI could publish bundle to server for session recovery.
      render json: { error: "Bundle not available. Scan QR code to connect." }, status: :not_found
    end

    private

    def set_hub
      @hub = current_user.hubs.find_by(identifier: params[:hub_identifier])
      render json: { error: "Hub not found" }, status: :not_found unless @hub
    end
  end
end
