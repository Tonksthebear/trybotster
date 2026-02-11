class DropSolidMCPMessages < ActiveRecord::Migration[8.1]
  def up
    drop_table :solid_mcp_messages, if_exists: true
  end

  def down
    create_table :solid_mcp_messages do |t|
      t.text :session_id, null: false
      t.text :event_type, null: false
      t.text :data
      t.datetime :delivered_at
      t.timestamps
    end
  end
end
