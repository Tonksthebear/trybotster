# frozen_string_literal: true

# Serves the React SPA shell for all frontend routes.
class SpaController < ApplicationController
  layout "spa"

  # Public — no auth required
  def home
    render "spa/show"
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
