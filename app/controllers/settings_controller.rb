# frozen_string_literal: true

class SettingsController < ApplicationController
  layout "application"
  before_action :authenticate_user!

  # GET /settings
  def show
  end
end
