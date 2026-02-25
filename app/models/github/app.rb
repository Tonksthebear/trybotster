# frozen_string_literal: true

module Github
  # Model for handling GitHub App authentication and API operations using Octokit
  # This is project-specific logic for interacting with GitHub as a GitHub App
  class App
  GITHUB_OAUTH_URL = "https://github.com/login/oauth"

  class << self
    # Get GitHub App ID from environment
    # @return [String] The GitHub App ID
    def app_id
      ENV["GITHUB_APP_ID"]
    end

    # Get private key for installation tokens
    # @return [String] The private key content
    def private_key
      if ENV["GITHUB_APP_PRIVATE_KEY"].present?
        ENV["GITHUB_APP_PRIVATE_KEY"]
      elsif ENV["GITHUB_APP_PRIVATE_KEY_PATH"].present?
        File.read(ENV["GITHUB_APP_PRIVATE_KEY_PATH"])
      else
        path = Rails.root.join("config/github_app_private_key.pem")
        File.read(path) if File.exist?(path)
      end
    end

    # Check if the GitHub App is installed on a repository.
    # Uses the App's private key (JWT auth) — no user OAuth token needed.
    # @param repo [String] Repository in "owner/repo" format
    # @return [Boolean] true if the App is installed on the repo
    def app_installed_on_repo?(repo)
      return false unless app_id && private_key

      app_client = Octokit::Client.new(bearer_token: generate_jwt)
      app_client.find_repository_installation(repo)
      true
    rescue Octokit::NotFound
      false
    rescue => e
      Rails.logger.warn "[Github::App] Installation check failed for #{repo}: #{e.message}"
      false
    end

    # Get the installation ID for a repository using App JWT auth.
    # No user OAuth token needed — uses the App's private key.
    # @param repo [String] Repository in "owner/repo" format
    # @return [Integer, nil] Installation ID or nil if not installed
    def installation_id_for_repo(repo)
      return nil unless app_id && private_key

      app_client = Octokit::Client.new(bearer_token: generate_jwt)
      installation = app_client.find_repository_installation(repo)
      installation.id
    rescue Octokit::NotFound
      nil
    rescue => e
      Rails.logger.warn "[Github::App] Installation lookup failed for #{repo}: #{e.message}"
      nil
    end

    # Get the first available installation ID (for non-repo-specific operations like search).
    # @return [Integer, nil]
    def first_installation_id
      return nil unless app_id && private_key

      app_client = Octokit::Client.new(bearer_token: generate_jwt)
      installations = app_client.find_app_installations
      installations.first&.id
    rescue => e
      Rails.logger.warn "[Github::App] Failed to find installations: #{e.message}"
      nil
    end

    # List all repositories accessible to the GitHub App across all installations.
    # Uses App JWT auth — no user OAuth token needed.
    # @return [Array<Hash>] Array of repository hashes
    def list_installation_repos
      return [] unless app_id && private_key

      app_client = Octokit::Client.new(bearer_token: generate_jwt)
      installations = app_client.find_app_installations

      repos = []
      installations.each do |inst|
        token_result = get_installation_token(inst.id)
        next unless token_result[:success]

        inst_client = Octokit::Client.new(access_token: token_result[:token])
        inst_repos = inst_client.list_app_installation_repositories
        repos.concat(inst_repos.repositories.map(&:to_h))
      end
      repos
    rescue => e
      Rails.logger.warn "[Github::App] Failed to list installation repos: #{e.message}"
      []
    end

    # Get installation token for acting as the app (shows [bot] badge)
    # @param installation_id [String] The installation ID
    # @return [Hash] Response with :success, :token, :expires_at, :error
    def get_installation_token(installation_id)
      return { success: false, error: "Missing GITHUB_APP_ID" } unless app_id
      return { success: false, error: "Missing private key" } unless private_key

      app_client = Octokit::Client.new(bearer_token: generate_jwt)
      token_response = app_client.create_app_installation_access_token(installation_id)

      {
        success: true,
        token: token_response.token,
        expires_at: token_response.expires_at.is_a?(Time) ? token_response.expires_at : Time.parse(token_response.expires_at)
      }
    rescue => e
      Rails.logger.error "GitHub App installation token error: #{e.message}"
      { success: false, error: e.message }
    end

    # Get installation client (acts as bot)
    # @param installation_id [String] The installation ID
    # @return [Octokit::Client] Configured Octokit client acting as installation
    def installation_client(installation_id)
      token_result = get_installation_token(installation_id)
      raise "Failed to get installation token: #{token_result[:error]}" unless token_result[:success]

      Octokit::Client.new(access_token: token_result[:token])
    end

    # Generate the GitHub App authorization URL
    # @param state [String] CSRF protection state parameter
    # @return [String] The authorization URL
    def authorization_url(state:)
      params = {
        client_id: client_id,
        state: state,
        redirect_uri: callback_url
      }.compact

      "#{GITHUB_OAUTH_URL}/authorize?#{URI.encode_www_form(params)}"
    end

    # Exchange authorization code for access token
    # @param code [String] The authorization code from GitHub
    # @return [Hash] Response with :success, :access_token, :refresh_token, :expires_at, :error
    def exchange_code_for_token(code)
      response = Faraday.post(
        "#{GITHUB_OAUTH_URL}/access_token",
        {
          client_id: client_id,
          client_secret: client_secret,
          code: code,
          redirect_uri: callback_url
        },
        { "Accept" => "application/json" }
      )

      if response.success?
        parse_token_response(JSON.parse(response.body))
      else
        {
          success: false,
          error: "Token exchange failed: #{response.status}"
        }
      end
    rescue => e
      Rails.logger.error "GitHub App token exchange error: #{e.message}"
      { success: false, error: e.message }
    end

    # Refresh an expired access token
    # @param refresh_token [String] The refresh token
    # @return [Hash] Response with :success, :access_token, :refresh_token, :expires_at, :error
    def refresh_token(refresh_token)
      response = Faraday.post(
        "#{GITHUB_OAUTH_URL}/access_token",
        {
          client_id: client_id,
          client_secret: client_secret,
          grant_type: "refresh_token",
          refresh_token: refresh_token
        },
        { "Accept" => "application/json" }
      )

      if response.success?
        parse_token_response(JSON.parse(response.body))
      else
        {
          success: false,
          error: "Token refresh failed: #{response.status}"
        }
      end
    rescue => e
      Rails.logger.error "GitHub App token refresh error: #{e.message}"
      { success: false, error: e.message }
    end

    # Get Octokit client for a given access token (user attribution)
    # @param access_token [String] The GitHub access token
    # @return [Octokit::Client] Configured Octokit client
    def client(access_token)
      Octokit::Client.new(access_token: access_token)
    end

    # Get user information from GitHub
    # @param access_token [String] The GitHub access token
    # @return [Hash] User information or error
    def get_user_info(access_token)
      client = client(access_token)
      user = client.user

      {
        success: true,
        user: user.to_h
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App user info error: #{e.message}"
      { success: false, error: e.message }
    end

    # Check if a user is a collaborator on a repository.
    # Uses installation token — fails closed (returns false on error).
    # @param installation_id [Integer] The GitHub App installation ID
    # @param repo [String] Repository in "owner/repo" format
    # @param username [String] GitHub username to check
    # @return [Boolean] true if the user is a collaborator
    def repo_collaborator?(installation_id, repo, username)
      client = installation_client(installation_id)
      client.collaborator?(repo, username)
    rescue Octokit::Error => e
      Rails.logger.warn "[Github::App] Collaborator check failed for #{username} on #{repo}: #{e.message}"
      false
    end

    # Add a reaction to an issue comment (using installation token - shows as bot)
    # @param installation_id [Integer] The GitHub App installation ID
    # @param repo [String] Repository in "owner/repo" format
    # @param comment_id [Integer] The comment ID to react to
    # @param reaction [String] Reaction type: +1, -1, laugh, confused, heart, hooray, rocket, eyes
    # @return [Hash] Reaction data or error
    def create_comment_reaction(installation_id, repo:, comment_id:, reaction:)
      client = installation_client(installation_id)
      result = client.create_issue_comment_reaction(repo, comment_id, reaction)

      {
        success: true,
        reaction: result.to_h
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App create reaction error: #{e.message}"
      { success: false, error: e.message }
    end

    # Add a reaction to an issue (using installation token - shows as bot)
    # @param installation_id [Integer] The GitHub App installation ID
    # @param repo [String] Repository in "owner/repo" format
    # @param issue_number [Integer] The issue number to react to
    # @param reaction [String] Reaction type: +1, -1, laugh, confused, heart, hooray, rocket, eyes
    # @return [Hash] Reaction data or error
    def create_issue_reaction(installation_id, repo:, issue_number:, reaction:)
      client = installation_client(installation_id)
      result = client.create_issue_reaction(repo, issue_number, reaction)

      {
        success: true,
        reaction: result.to_h
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App create issue reaction error: #{e.message}"
      { success: false, error: e.message }
    end

    private

    # Generate a JWT for GitHub App authentication.
    # Uses the App's private key (RS256). Valid for 10 minutes.
    # @return [String] Encoded JWT
    def generate_jwt
      payload = {
        iat: Time.now.to_i - 60,
        exp: Time.now.to_i + (10 * 60),
        iss: app_id.to_s
      }
      JWT.encode(payload, OpenSSL::PKey::RSA.new(private_key), "RS256")
    end

    # Parse token response from GitHub
    def parse_token_response(data)
      if data["error"]
        return {
          success: false,
          error: data["error_description"] || data["error"]
        }
      end

      {
        success: true,
        access_token: data["access_token"],
        refresh_token: data["refresh_token"],
        expires_in: data["expires_in"],
        expires_at: data["expires_in"] ? data["expires_in"].seconds.from_now : nil,
        token_type: data["token_type"],
        scope: data["scope"]
      }
    end

    # GitHub App client ID from environment
    def client_id
      ENV["GITHUB_APP_CLIENT_ID"] || ENV["GITHUB_CLIENT_ID"]
    end

    # GitHub App client secret from environment
    def client_secret
      ENV["GITHUB_APP_CLIENT_SECRET"] || ENV["GITHUB_CLIENT_SECRET"]
    end

    # OAuth callback URL
    def callback_url
      ENV["GITHUB_APP_CALLBACK_URL"] || "https://#{ENV['HOST_URL']}/github/callback"
    end
  end
  end
end
