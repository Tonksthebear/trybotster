class User < ApplicationRecord
  # Minimal modules: No :database_authenticatable, :registerable, :recoverable, :validatable
  # Note: :omniauthable not used - we use custom GitHub App OAuth instead
  devise :trackable, :rememberable

  encrypts :api_key, deterministic: true
  encrypts :github_app_token, deterministic: true
  encrypts :github_app_refresh_token, deterministic: true

  # Associations
  belongs_to :team, optional: true

  # Skip email/password validations for OAuth users
  validates :email, presence: true, uniqueness: true, if: -> { provider.blank? }  # Only if not OAuth
  validates :username, presence: true, uniqueness: true, allow_blank: true  # Optional

  # Generate API token before create
  before_create :generate_api_key
  after_create :ensure_api_key

  # Note: No active/inactive status - all users receive @trybotster mentions
  # Filtering happens at poll time based on repo access

  # Enhanced from_omniauth: Handle email fallback if GitHub doesn't provide it
  def self.from_omniauth(auth)
    where(provider: auth.provider, uid: auth.uid).first_or_initialize do |user|
      user.email = auth.info.email.presence || "#{auth.info.nickname || 'user'}@github-fallback.com"
      user.username = auth.info.nickname || auth.info.name
      # No password set (not needed)
    end.tap do |user|
      # Merge with existing local user by email (if any, for migration)
      if existing_user = User.find_by(email: user.email)
        existing_user.update(provider: auth.provider, uid: auth.uid, username: user.username)
        user = existing_user
      else
        user.save!
      end
    end
  end

  def after_sign_in_path
    root_path
  end

  # Generate a secure API token for MCP access
  def generate_api_key
    self.api_key = SecureRandom.urlsafe_base64(32)
  end

  # Ensure API key exists (used in after_create callback)
  def ensure_api_key
    regenerate_api_key! unless api_key.present?
  end

  # Regenerate API token (for security purposes)
  def regenerate_api_key!
    generate_api_key
    save!
  end

  # Check if user is active (default to true if column doesn't exist)
  def active?
    respond_to?(:active) ? active : true
  end

  # Touch last login timestamp
  def touch_last_login!
    touch(:last_sign_in_at) if respond_to?(:last_sign_in_at)
  end

  # GitHub App Authorization Methods



  # Check if user has authorized the GitHub App
  def github_app_authorized?
    github_app_token.present?
  end

  # Check if the GitHub App token is expired or about to expire
  def github_app_token_expired?
    return true if github_app_token_expires_at.blank?
    github_app_token_expires_at <= Time.current
  end

  # Check if the GitHub App token needs refresh (expires in less than 1 hour)
  def github_app_token_needs_refresh?
    return true if github_app_token_expires_at.blank?
    github_app_token_expires_at <= 1.hour.from_now
  end

  # Get a valid GitHub App token, refreshing if necessary
  def valid_github_app_token
    return nil unless github_app_authorized?

    if github_app_token_needs_refresh?
      refresh_github_app_token!
    end

    github_app_token
  end

  # Refresh the GitHub App token using the refresh token
  def refresh_github_app_token!
    return false unless github_app_refresh_token.present?

    response = Github::App.refresh_token(github_app_refresh_token)

    if response[:success]
      update!(
        github_app_token: response[:access_token],
        github_app_refresh_token: response[:refresh_token],
        github_app_token_expires_at: response[:expires_at]
      )

      true
    else
      Rails.logger.error "Failed to refresh GitHub App token: #{response[:error]}"
      false
    end
  rescue => e
    Rails.logger.error "Error refreshing GitHub App token: #{e.message}"
    false
  end

  # Revoke GitHub App authorization
  def revoke_github_app_authorization!
    update!(
      github_app_token: nil,
      github_app_refresh_token: nil,
      github_app_token_expires_at: nil,
      github_app_permissions: {}
    )
  end

  # Check if user has access to a GitHub repository
  # @param repo_full_name [String] Repository full name (e.g., "owner/repo")
  # @return [Boolean] true if user has access, false otherwise
  def has_github_repo_access?(repo_full_name)
    return false unless github_app_authorized?

    token = valid_github_app_token
    return false if token.blank?

    # Use GitHub API to check if user has access to the repository
    client = Octokit::Client.new(access_token: token)

    begin
      # Try to fetch the repository - if user has access, this succeeds
      client.repository(repo_full_name)
      true
    rescue Octokit::NotFound
      # User doesn't have access or repo doesn't exist
      Rails.logger.info "User #{id} (#{username}) does not have access to #{repo_full_name}"
      false
    rescue Octokit::Unauthorized, Octokit::Forbidden
      # Token is invalid or doesn't have required permissions
      Rails.logger.warn "User #{id} (#{username}) has invalid/expired token for #{repo_full_name}"
      false
    rescue => e
      # Other errors (rate limit, network issues, etc.)
      Rails.logger.error "Error checking repo access for user #{id}: #{e.message}"
      false
    end
  end
end
