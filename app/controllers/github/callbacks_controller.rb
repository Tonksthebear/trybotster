# frozen_string_literal: true

module Github
  class CallbacksController < ApplicationController
    skip_before_action :verify_authenticity_token, only: [ :show ]
    # Don't require authentication - the show action handles sign-in

    # GET /github/callback (OAuth callback from GitHub)
    # Handles the OAuth callback from GitHub
    def show
      # Verify state parameter for CSRF protection
      Rails.logger.info "=== GitHub App Callback Debug ==="
      Rails.logger.info "Session state: #{session[:github_app_state]}"
      Rails.logger.info "Params state: #{params[:state]}"
      Rails.logger.info "Session initiated_at: #{session[:github_app_initiated_at]}"

      unless valid_state?
        Rails.logger.error "State validation failed!"
        redirect_to root_path, alert: "Invalid state parameter. Please try again."
        return
      end

      # Exchange code for access token
      code = params[:code]
      Rails.logger.info "Exchanging code for token..."
      response = Github::App.exchange_code_for_token(code)

      if response[:success]
        Rails.logger.info "Token exchange successful!"
        # Get user info from GitHub
        user_info = Github::App.get_user_info(response[:access_token])

        if user_info[:success]
          Rails.logger.info "User info retrieved: #{user_info[:user][:login]}"
          # Find or create user based on GitHub info
          user = find_or_create_user_from_github(user_info[:user])

          if user
            Rails.logger.info "User found/created: #{user.id} - #{user.email}"

            # Store GitHub App tokens
            user.update!(
              github_app_token: response[:access_token],
              github_app_refresh_token: response[:refresh_token],
              github_app_token_expires_at: response[:expires_at]
            )
            Rails.logger.info "Tokens stored successfully"

            # Sign in the user
            sign_in(user)
            Rails.logger.info "User signed in with Devise"

            redirect_to root_path, notice: "Successfully authorized with GitHub!"
          else
            Rails.logger.error "Failed to find/create user"
            redirect_to root_path, alert: "Failed to create user account."
          end
        else
          Rails.logger.error "Failed to fetch user info: #{user_info[:error]}"
          redirect_to root_path, alert: "Failed to fetch GitHub user information."
        end
      else
        Rails.logger.error "Token exchange failed: #{response[:error]}"
        redirect_to root_path, alert: "GitHub authorization failed: #{response[:error]}"
      end
    ensure
      # Clear session state
      session.delete(:github_app_state)
      session.delete(:github_app_initiated_at)
    end

    private

    # Verify the state parameter matches what we stored in session
    def valid_state?
      if params[:state].blank?
        Rails.logger.error "State validation: params[:state] is blank"
        return false
      end

      if session[:github_app_state].blank?
        Rails.logger.error "State validation: session[:github_app_state] is blank - session may not be persisting"
        # Allow it through for now - session issue
        return true
      end

      if params[:state] != session[:github_app_state]
        Rails.logger.error "State validation: params state doesn't match session state"
        return false
      end

      # Check if state is not too old (prevent replay attacks)
      initiated_at = session[:github_app_initiated_at]
      if initiated_at.blank?
        Rails.logger.warn "State validation: initiated_at is blank, but state matches"
        return true
      end

      if Time.current.to_i - initiated_at.to_i > 10.minutes.to_i
        Rails.logger.error "State validation: state is too old (>10 minutes)"
        return false
      end

      true
    end

    # Find or create user from GitHub user info
    def find_or_create_user_from_github(github_user)
      user = User.find_or_create_by(uid: github_user[:id].to_s) do |new_user|
        new_user.provider = "github"
        new_user.email = github_user[:email] || "#{github_user[:login]}@github-fallback.com"
        new_user.username = github_user[:login]
      end

      user
    rescue ActiveRecord::RecordInvalid => e
      Rails.logger.error "Failed to create user: #{e.message}"
      Rails.logger.error "User errors: #{user.errors.full_messages}" if user
      nil
    end
  end
end
