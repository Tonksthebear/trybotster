class CreateHubAgents < ActiveRecord::Migration[8.1]
  def change
    create_table :hub_agents do |t|
      t.references :hub, null: false, foreign_key: true
      t.string :session_key, null: false
      t.string :last_invocation_url

      t.timestamps
    end
    add_index :hub_agents, [ :hub_id, :session_key ], unique: true
  end
end
