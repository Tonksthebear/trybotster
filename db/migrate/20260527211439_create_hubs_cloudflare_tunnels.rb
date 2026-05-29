# frozen_string_literal: true

class CreateHubsCloudflareTunnels < ActiveRecord::Migration[8.1]
  def change
    create_table :hubs_cloudflare_tunnels do |t|
      t.references :hub, null: false, foreign_key: true, index: { unique: true }
      t.string :cloudflare_tunnel_id, null: false
      t.string :cloudflare_tunnel_name, null: false
      t.string :tunnel_secret
      t.string :token_secret
      t.integer :token_version, null: false, default: 0
      t.datetime :token_delivered_at
      t.datetime :token_expires_at
      t.string :status, null: false, default: "pending"
      t.text :last_error
      t.datetime :last_synced_at

      t.timestamps
    end

    add_index :hubs_cloudflare_tunnels, :cloudflare_tunnel_id, unique: true
    add_index :hubs_cloudflare_tunnels, :cloudflare_tunnel_name
    add_index :hubs_cloudflare_tunnels, :status
  end
end
