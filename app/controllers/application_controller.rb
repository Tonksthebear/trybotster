class ApplicationController < ActionController::Base
  # Only allow modern browsers supporting webp images, web push, badges, import maps, CSS nesting, and CSS :has.
  allow_browser versions: :modern

  # Changes to the importmap will invalidate the etag for HTML responses
  stale_when_importmap_changes

  # CSRF protection strategy:
  # - Browser requests (session auth): raise exception on invalid token
  # - API requests (Bearer token auth): null session (clears session, no exception)
  #
  # This follows the Fizzy pattern: Bearer tokens in Authorization header are
  # inherently CSRF-safe since browsers don't auto-send them.
  protect_from_forgery with: :exception, unless: :bearer_token_request?

  layout :choose_layout

  before_action :set_sidebar_hubs, if: :user_signed_in?

  private

  def choose_layout
    user_signed_in? ? "sidebar" : "application"
  end

  def set_sidebar_hubs
    @sidebar_hubs = current_user.hubs.includes(:device).order(last_seen_at: :desc)
  end

  def bearer_token_request?
    request.headers["Authorization"]&.start_with?("Bearer ")
  end
end
