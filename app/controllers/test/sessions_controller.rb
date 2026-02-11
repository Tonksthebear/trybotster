# frozen_string_literal: true

# Test-only session controller for signing in users during system tests.
# This bypasses OAuth flow and allows direct authentication.
#
# IMPORTANT: Only works in test environment.
module Test
  class SessionsController < ApplicationController
    skip_before_action :verify_authenticity_token

    # POST /test/sessions - for HTTP requests
    def create
      unless Rails.env.test?
        render plain: "Not available", status: :forbidden
        return
      end

      user = User.find(params[:user_id])
      sign_in(user)
      render plain: "OK", status: :ok
    end

    # GET /test/sessions/new?user_id=123 - for browser redirect
    def new
      unless Rails.env.test?
        redirect_to root_path
        return
      end

      user = User.find(params[:user_id])
      sign_in(user)

      # Redirect to the return URL or root
      redirect_to params[:return_to] || root_path
    end
  end
end
