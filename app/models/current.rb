class Current < ActiveSupport::CurrentAttributes
  attribute :user, :hub, :agent_index, :pty_index
end
