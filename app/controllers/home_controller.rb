class HomeController < ApplicationController
  before_action :set_user_data, if: :user_signed_in?

  def index
  end

  private

  def set_user_data
    @api_key = current_user.api_key
    @user_data = { username: current_user.username, email: current_user.email }
  end
end
