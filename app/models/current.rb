class Current < ActiveSupport::CurrentAttributes
  attribute :user, :hub, :agent, :pty
end
