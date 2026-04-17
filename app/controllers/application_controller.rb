class ApplicationController < ActionController::Base
  # Only allow modern browsers supporting webp images, web push, badges, CSS nesting, and CSS :has.
  allow_browser versions: :modern

  # CSRF protection strategy:
  # - Browser requests (session auth): raise exception on invalid token
  # - API requests (Bearer token auth): null session (clears session, no exception)
  #
  # This follows the Fizzy pattern: Bearer tokens in Authorization header are
  # inherently CSRF-safe since browsers don't auto-send them.
  protect_from_forgery with: :exception, unless: :bearer_token_request?

  layout :choose_layout

  before_action :set_current_attributes, if: :user_signed_in?

  private

  def choose_layout
    "spa"
  end

  def set_current_attributes
    Current.user = current_user
  end

  def bearer_token_request?
    request.headers["Authorization"]&.start_with?("Bearer ")
  end
end
