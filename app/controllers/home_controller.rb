class HomeController < ApplicationController
  def index
    if user_signed_in?
      # Ensure user has an API token
      current_user.regenerate_api_key! unless current_user.api_key.present?

      @api_key = current_user.api_key
      @user_data = { username: current_user.username, email: current_user.email }
    end
  end
end
