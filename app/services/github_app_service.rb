# frozen_string_literal: true

# Service for handling GitHub App authentication and API operations using Octokit
class GithubAppService
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

    # Get installation token for acting as the app (shows [bot] badge)
    # @param installation_id [String] The installation ID
    # @return [Hash] Response with :success, :token, :expires_at, :error
    def get_installation_token(installation_id)
      return { success: false, error: "Missing GITHUB_APP_ID" } unless app_id
      return { success: false, error: "Missing private key" } unless private_key

      # Create JWT for app authentication
      payload = {
        iat: Time.now.to_i - 60, # issued at time, 60 seconds in the past to allow for clock drift
        exp: Time.now.to_i + (10 * 60), # JWT expiration time (10 minute maximum)
        iss: app_id.to_s # GitHub expects this as a string
      }

      jwt = JWT.encode(payload, OpenSSL::PKey::RSA.new(private_key), "RS256")

      # Get installation token using JWT
      app_client = Octokit::Client.new(bearer_token: jwt)
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

    # Get the installation ID for a user
    # @param access_token [String] The user's access token
    # @return [Hash] Response with :success, :installation_id, :permissions, :error
    def get_user_installation(access_token)
      client = client(access_token)
      installations = client.find_user_installations

      if installations.total_count > 0
        # Prefer personal account installation over org installations
        installation = installations.installations.find { |i| i.account.type == "User" } || installations.installations.first
        {
          success: true,
          installation_id: installation.id,
          account: installation.account.login,
          account_type: installation.account.type,
          permissions: installation.permissions&.to_h || {}
        }
      else
        {
          success: false,
          error: "No installation found for this user"
        }
      end
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App installation lookup error: #{e.message}"
      { success: false, error: e.message }
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

    # Get user's repositories
    # @param access_token [String] The GitHub access token
    # @param options [Hash] Options like per_page, page, sort, etc.
    # @return [Hash] Repositories list or error
    def get_user_repos(access_token, **options)
      client = client(access_token)
      repos = client.repos(nil, options)

      {
        success: true,
        repos: repos.map(&:to_h)
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App repos error: #{e.message}"
      { success: false, error: e.message }
    end

    # Get user's issues
    # @param access_token [String] The GitHub access token
    # @param filter [String] Filter: assigned, created, mentioned, subscribed, all
    # @param state [String] State: open, closed, all
    # @return [Hash] Issues list or error
    def get_user_issues(access_token, filter: "assigned", state: "open")
      client = client(access_token)
      issues = client.issues(filter: filter, state: state)

      {
        success: true,
        issues: issues.map(&:to_h)
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App issues error: #{e.message}"
      { success: false, error: e.message }
    end

    # Get user's pull requests
    # @param access_token [String] The GitHub access token
    # @param state [String] State: open, closed, all
    # @return [Hash] Pull requests list or error
    def get_user_pull_requests(access_token, state: "open")
      client = client(access_token)
      query = "is:pr author:@me state:#{state}"
      result = client.search_issues(query, sort: "updated", order: "desc")

      {
        success: true,
        pull_requests: result.items.map(&:to_h)
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App PRs error: #{e.message}"
      { success: false, error: e.message }
    end

    # Create an issue comment
    # @param access_token [String] The GitHub access token
    # @param repo [String] Repository in "owner/repo" format
    # @param issue_number [Integer] Issue or PR number
    # @param body [String] Comment body
    # @return [Hash] Comment data or error
    def create_issue_comment(access_token, repo:, issue_number:, body:)
      client = client(access_token)
      comment = client.add_comment(repo, issue_number, body)

      {
        success: true,
        comment: comment.to_h
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App comment error: #{e.message}"
      { success: false, error: e.message }
    end

    # Create an issue
    # @param access_token [String] The GitHub access token
    # @param repo [String] Repository in "owner/repo" format
    # @param title [String] Issue title
    # @param body [String] Issue body
    # @param labels [Array<String>] Optional labels
    # @return [Hash] Issue data or error
    def create_issue(access_token, repo:, title:, body: nil, labels: [])
      client = client(access_token)
      issue = client.create_issue(repo, title, body, labels: labels)

      {
        success: true,
        issue: issue.to_h
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App create issue error: #{e.message}"
      { success: false, error: e.message }
    end

    # Get repository details
    # @param access_token [String] The GitHub access token
    # @param repo [String] Repository in "owner/repo" format
    # @return [Hash] Repository data or error
    def get_repo(access_token, repo:)
      client = client(access_token)
      repository = client.repository(repo)

      {
        success: true,
        repo: repository.to_h
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App get repo error: #{e.message}"
      { success: false, error: e.message }
    end

    # Get issue details
    # @param access_token [String] The GitHub access token
    # @param repo [String] Repository in "owner/repo" format
    # @param issue_number [Integer] Issue or PR number
    # @return [Hash] Issue data or error
    def get_issue(access_token, repo:, issue_number:)
      client = client(access_token)
      issue = client.issue(repo, issue_number)

      {
        success: true,
        issue: issue.to_h
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App get issue error: #{e.message}"
      { success: false, error: e.message }
    end

    # Search repositories
    # @param access_token [String] The GitHub access token
    # @param query [String] Search query
    # @param options [Hash] Options like sort, order, per_page
    # @return [Hash] Search results or error
    def search_repos(access_token, query:, **options)
      client = client(access_token)
      result = client.search_repositories(query, options)

      {
        success: true,
        repos: result.items.map(&:to_h),
        total_count: result.total_count
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App search repos error: #{e.message}"
      { success: false, error: e.message }
    end

    # Get file contents from repository
    # @param access_token [String] The GitHub access token
    # @param repo [String] Repository in "owner/repo" format
    # @param path [String] File path in repository
    # @param ref [String] Branch, tag, or commit (default: main branch)
    # @return [Hash] File content or error
    def get_file_contents(access_token, repo:, path:, ref: nil)
      client = client(access_token)
      options = ref ? { ref: ref } : {}
      content = client.contents(repo, path: path, **options)

      {
        success: true,
        content: content.to_h,
        decoded_content: Base64.decode64(content.content)
      }
    rescue Octokit::Error => e
      Rails.logger.error "GitHub App get file error: #{e.message}"
      { success: false, error: e.message }
    end

    private

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
      ENV["GITHUB_APP_CALLBACK_URL"] || "#{ENV['APP_URL']}/auth/github_app/callback"
    end

    # Determine which client to use based on user preference
    # @param user [User] The user object
    # @param use_bot [Boolean] Whether to use bot attribution (default: true)
    # @return [Octokit::Client] The appropriate client
    def client_for_user(user, use_bot: true)
      if use_bot && user.github_app_installation_id.present?
        # Use installation token (shows as [bot])
        installation_client(user.github_app_installation_id)
      else
        # Use user token (shows as user)
        token = user.valid_github_app_token
        client(token)
      end
    end
  end
end
