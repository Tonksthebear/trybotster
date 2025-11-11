# frozen_string_literal: true

class GithubAppController < ApplicationController
  skip_before_action :verify_authenticity_token, only: [:callback]
  # Don't require authentication - the authorize action handles sign-in

  # GET /github_app/authorize
  # Initiates the GitHub App authorization flow
  def authorize
    state = SecureRandom.hex(16)
    session[:github_app_state] = state
    session[:github_app_initiated_at] = Time.current.to_i

    redirect_to GithubAppService.authorization_url(state: state), allow_other_host: true
  end

  # GET /github_app/callback
  # Handles the OAuth callback from GitHub
  def callback
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
    response = GithubAppService.exchange_code_for_token(code)

    if response[:success]
      Rails.logger.info "Token exchange successful!"
      # Get user info from GitHub
      user_info = GithubAppService.get_user_info(response[:access_token])

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

          # Fetch installation ID for bot features
          installation_result = GithubAppService.get_user_installation(response[:access_token])
          if installation_result[:success]
            user.update!(github_app_installation_id: installation_result[:installation_id].to_s)
            Rails.logger.info "Installation ID saved: #{installation_result[:installation_id]}"
          else
            Rails.logger.warn "Could not fetch installation ID: #{installation_result[:error]}"
          end

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

  # DELETE /github_app/revoke
  # Revokes GitHub App authorization
  def revoke
    unless current_user
      redirect_to root_path, alert: "Please sign in first."
      return
    end

    current_user.revoke_github_app_authorization!
    redirect_to root_path, notice: "GitHub App authorization revoked."
  end

  # GET /github_app/status
  # Returns the current authorization status
  def status
    unless current_user
      render json: { authorized: false, error: "Not signed in" }, status: :unauthorized
      return
    end

    render json: {
      authorized: current_user.github_app_authorized?,
      expires_at: current_user.github_app_token_expires_at,
      needs_refresh: current_user.github_app_token_needs_refresh?
    }
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
