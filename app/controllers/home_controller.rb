class HomeController < ApplicationController
  def index
    redirect_to hubs_path if user_signed_in?
  end
end
