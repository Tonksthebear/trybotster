class Current < ActiveSupport::CurrentAttributes
  attribute :user, :hub, :session_uuid
end
