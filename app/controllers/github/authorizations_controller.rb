# frozen_string_literal: true

module Github
  class AuthorizationsController < ApplicationController
    # GET /github_authorization/new
    # Initiates the GitHub App authorization flow (RESTful: new)
    def new
      state = SecureRandom.hex(16)
      session[:github_app_state] = state
      session[:github_app_initiated_at] = Time.current.to_i

      redirect_to Github::App.authorization_url(state: state), allow_other_host: true
    end

    # DELETE /github_authorization
    # Revokes GitHub App authorization (RESTful: destroy)
    def destroy
      unless current_user
        redirect_to root_path, alert: "Please sign in first."
        return
      end

      current_user.revoke_github_app_authorization!
      redirect_to root_path, notice: "GitHub App authorization revoked."
    end
  end
end
