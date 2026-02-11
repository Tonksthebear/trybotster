# frozen_string_literal: true

class SettingsController < ApplicationController
  before_action :authenticate_user!

  # GET /settings
  def show
  end

  # PATCH /settings
  def update
    if current_user.update(user_params)
      redirect_to settings_path, notice: "Settings updated successfully."
    else
      render :show, status: :unprocessable_entity
    end
  end

  private

  def user_params
    params.require(:user).permit(:server_assisted_pairing)
  end
end
