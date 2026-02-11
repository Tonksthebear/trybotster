class AddTunnelPortToHubAgents < ActiveRecord::Migration[8.1]
  def change
    add_column :hub_agents, :tunnel_port, :integer
    add_column :hub_agents, :tunnel_status, :string, default: "disconnected"
    add_column :hub_agents, :tunnel_connected_at, :datetime
    add_column :hub_agents, :tunnel_last_request_at, :datetime

    # Public sharing support
    add_column :hub_agents, :tunnel_share_token, :string
    add_column :hub_agents, :tunnel_share_enabled, :boolean, default: false

    add_index :hub_agents, :tunnel_share_token, unique: true
  end
end
