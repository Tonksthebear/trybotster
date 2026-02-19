# frozen_string_literal: true

module Hubs
  class PairingController < ApplicationController
    before_action :authenticate_user!
    before_action :set_hub

    def show
    end

    private

    def set_hub
      Current.hub = current_user.hubs.find_by(id: params[:hub_id])
      redirect_to hubs_path, alert: "Hub not found" unless Current.hub
    end
  end
end
