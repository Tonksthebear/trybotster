class AddHubIdAndSequenceToBotMessages < ActiveRecord::Migration[8.1]
  def change
    add_reference :bot_messages, :hub, foreign_key: true, null: true
    add_column :bot_messages, :sequence, :bigint
    add_index :bot_messages, [ :hub_id, :sequence ], unique: true,
              where: "hub_id IS NOT NULL AND sequence IS NOT NULL"
    add_column :hubs, :message_sequence, :bigint, default: 0, null: false
  end
end
