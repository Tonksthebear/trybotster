# frozen_string_literal: true

class DropBotMessages < ActiveRecord::Migration[8.0]
  def up
    drop_table :bot_messages
  end

  def down
    create_table :bot_messages do |t|
      t.datetime :acknowledged_at
      t.datetime :claimed_at
      t.bigint :claimed_by_user_id
      t.string :event_type, null: false
      t.bigint :hub_id
      t.jsonb :payload, default: {}, null: false
      t.datetime :sent_at
      t.bigint :sequence
      t.string :status, default: "pending", null: false

      t.timestamps
    end

    add_index :bot_messages, :acknowledged_at
    add_index :bot_messages, :claimed_at
    add_index :bot_messages, :claimed_by_user_id
    add_index :bot_messages, :event_type
    add_index :bot_messages, [ :hub_id, :sequence ], unique: true, where: "((hub_id IS NOT NULL) AND (sequence IS NOT NULL))"
    add_index :bot_messages, :hub_id
    add_index :bot_messages, :sent_at
    add_index :bot_messages, :status
  end
end
