class AddTailscalePreauthKeyToHubs < ActiveRecord::Migration[8.1]
  def change
    add_column :hubs, :tailscale_preauth_key, :string
    add_column :hubs, :tailscale_hostname, :string  # CLI's tailnet hostname (e.g., "cli-abc123.tail.local")
  end
end
