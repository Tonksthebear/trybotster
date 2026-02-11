class AddUniqueIndexToBotMessagesForBrowserConnected < ActiveRecord::Migration[8.1]
  def change
    # Prevent duplicate browser_connected/terminal_connected messages for the same browser.
    # Only enforced for pending/sent messages (acknowledged ones can be duplicated).
    # Uses a partial unique index on the JSONB payload's browser_identity field.
    add_index :bot_messages,
      "hub_id, event_type, (payload->>'browser_identity')",
      unique: true,
      where: "status IN ('pending', 'sent') AND event_type IN ('browser_connected', 'terminal_connected', 'browser_wants_preview')",
      name: "idx_bot_messages_unique_pending_browser_identity"
  end
end
