# frozen_string_literal: true

module Hubs
  # Pairing is a public route — the URL fragment carries the cryptographic
  # bundle, which is the credential. Requiring authenticate_user! before
  # rendering forced an OAuth bounce that stripped the fragment in the
  # redirect (browsers don't send fragments to the server, and Devise's
  # stored_location_for path doesn't preserve them across the GitHub
  # round-trip). The SPA validates the bundle client-side and only then
  # establishes session state.
  class PairingController < ApplicationController
    before_action :set_hub

    def show
      render "spa/show", layout: "spa"
    end

    private

    def set_hub
      Current.hub = Hub.find_by(id: params[:hub_id])
    end
  end
end
