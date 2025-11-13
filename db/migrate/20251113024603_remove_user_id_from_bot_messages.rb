class RemoveUserIdFromBotMessages < ActiveRecord::Migration[8.1]
  def change
    # Remove foreign key constraint first
    remove_foreign_key :bot_messages, :users if foreign_key_exists?(:bot_messages, :users)

    # Remove the column and its indexes
    remove_column :bot_messages, :user_id, :bigint

    # Add claimed_by_user_id to track which user/daemon claimed the message
    # Note: We don't add a foreign key constraint so we can track claims even if user is deleted
    add_column :bot_messages, :claimed_by_user_id, :bigint
    add_column :bot_messages, :claimed_at, :datetime

    # Add indexes for efficient querying
    add_index :bot_messages, :claimed_at
    add_index :bot_messages, :claimed_by_user_id
  end
end
