# frozen_string_literal: true

class CreateHubCommands < ActiveRecord::Migration[8.0]
  def change
    create_table :hub_commands do |t|
      t.references :hub, null: false, foreign_key: true
      t.string :event_type, null: false
      t.jsonb :payload, null: false, default: {}
      t.string :status, null: false, default: "pending"
      t.bigint :sequence, null: false
      t.datetime :acknowledged_at

      t.timestamps
    end

    add_index :hub_commands, [ :hub_id, :sequence ], unique: true
    add_index :hub_commands, :status
  end
end
