class RemoveUniqueIndexFromBotMessagesBrowserIdentity < ActiveRecord::Migration[8.1]
  def change
    # Remove the unique constraint - it was blocking legitimate reconnection attempts.
    # CLI can handle duplicate messages (it's idempotent).
    remove_index :bot_messages,
      name: "idx_bot_messages_unique_pending_browser_identity",
      if_exists: true
  end
end
