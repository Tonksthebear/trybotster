# frozen_string_literal: true

module ApplicationCable
  class Connection < ActionCable::Connection::Base
    identified_by :current_user

    def connect
      self.current_user = find_verified_user
    end

    private

    def find_verified_user
      # Try API key auth (for CLI)
      if (api_key = request.params[:api_key])
        if (user = User.find_by_api_key(api_key))
          return user
        end
      end

      # Try session auth (for browser)
      if (user = env["warden"].user)
        return user
      end

      reject_unauthorized_connection
    end
  end
end
