class CreateBotMessages < ActiveRecord::Migration[8.1]
  def change
    create_table :bot_messages do |t|
      t.references :user, null: false, foreign_key: true
      t.string :event_type, null: false
      t.jsonb :payload, null: false, default: {}
      t.datetime :sent_at
      t.datetime :acknowledged_at
      t.string :status, null: false, default: "pending" # pending, sent, acknowledged, failed

      t.timestamps
    end

    # Indexes for efficient querying
    add_index :bot_messages, :event_type
    add_index :bot_messages, :status
    add_index :bot_messages, [ :user_id, :status ]
    add_index :bot_messages, :sent_at
    add_index :bot_messages, :acknowledged_at
    add_index :bot_messages, [ :user_id, :created_at ]
  end
end
