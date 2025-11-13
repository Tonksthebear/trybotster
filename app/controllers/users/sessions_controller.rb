class Users::SessionsController < Devise::SessionsController
  def destroy
    # Sign out the user
    signed_out = sign_out(current_user)

    # Set flash message for web
    set_flash_message! :notice, :signed_out if signed_out

    # Respond based on format
    respond_to do |format|
      format.json do
        render json: { message: "Logged out successfully" }, status: :ok
      end
      format.any { redirect_to after_sign_out_path_for(resource_name) }
    end
  end
end
