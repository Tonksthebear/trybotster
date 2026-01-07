class RemoveSharingColumnsFromHubAgents < ActiveRecord::Migration[8.1]
  def change
    remove_column :hub_agents, :tunnel_share_token, :string
    remove_column :hub_agents, :tunnel_share_enabled, :boolean
  end
end
