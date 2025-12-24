# frozen_string_literal: true

class TunnelSharesController < ApplicationController
  before_action :authenticate_user!
  before_action :find_hub_agent

  # POST /tunnel_shares
  def create
    @hub_agent.enable_sharing!
    respond_to do |format|
      format.html { redirect_back fallback_location: hubs_path, notice: "Share link created" }
      format.json { render json: { share_url: @hub_agent.share_url } }
      format.turbo_stream
    end
  end

  # DELETE /tunnel_shares/:hub_agent_id
  def destroy
    @hub_agent.disable_sharing!
    respond_to do |format|
      format.html { redirect_back fallback_location: hubs_path, notice: "Share link disabled" }
      format.json { render json: { success: true } }
      format.turbo_stream
    end
  end

  private

  def find_hub_agent
    hub_agent_id = params[:hub_agent_id] || params[:id]
    @hub_agent = HubAgent.joins(:hub)
                         .where(hubs: { user_id: current_user.id })
                         .find_by(id: hub_agent_id)

    render_not_found unless @hub_agent
  end

  def render_not_found
    respond_to do |format|
      format.html { redirect_back fallback_location: hubs_path, alert: "Agent not found" }
      format.json { render json: { error: "Not found" }, status: :not_found }
    end
  end
end
