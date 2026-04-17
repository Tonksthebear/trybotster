# frozen_string_literal: true

# Serves the React SPA shell for all frontend routes.
class SpaController < ApplicationController
  layout "spa"

  # Public — signed-in visitors jump straight to their hubs dashboard.
  def home
    if user_signed_in?
      redirect_to hubs_path
    else
      render "spa/show"
    end
  end

  def docs
    render "spa/show"
  end

  # Authenticated
  def hubs
    authenticate_user!
    render "spa/show"
  end

  def hub
    authenticate_user!
    render "spa/show"
  end
end
