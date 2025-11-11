class Users::OmniauthCallbacksController < Devise::OmniauthCallbacksController
  include JwtAuthenticatable::InstanceMethods

  def github
    @user = User.from_omniauth(request.env["omniauth.auth"])

    if @user.persisted? || @user.save  # Handle first-time creation
      sign_in_and_redirect @user, event: :authentication
      set_flash_message(:notice, :success, kind: "GitHub") if is_navigational_format?
    else
      # Log errors for debugging; redirect with alert
      Rails.logger.error "OAuth failure: #{@user.errors.full_messages}"
      redirect_to root_path, alert: "GitHub authentication failed. Please try again."
    end
  end

  def failure
    redirect_to root_path
  end

  # Callback method required by OmniAuth
  def passthru
    super
  end

  protected

  def after_omniauth_failure_path_for(_scope)
    new_user_session_url
  end
end
