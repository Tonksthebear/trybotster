class DropHubAgents < ActiveRecord::Migration[8.1]
  def up
    drop_table :hub_agents
  end

  def down
    create_table :hub_agents do |t|
      t.references :hub, null: false, foreign_key: true
      t.string :session_key, null: false
      t.string :last_invocation_url
      t.integer :tunnel_port
      t.string :tunnel_status, default: "disconnected"
      t.datetime :tunnel_connected_at
      t.datetime :tunnel_last_request_at
      t.timestamps
    end

    add_index :hub_agents, [ :hub_id, :session_key ], unique: true
  end
end
