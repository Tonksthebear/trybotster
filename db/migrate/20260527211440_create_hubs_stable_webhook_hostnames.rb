# frozen_string_literal: true

class CreateHubsStableWebhookHostnames < ActiveRecord::Migration[8.1]
  def change
    create_table :hubs_stable_webhook_hostnames do |t|
      t.references :hub, null: false, foreign_key: true
      t.references :hubs_cloudflare_tunnel, null: false, foreign_key: true, index: false
      t.string :hostname, null: false
      t.string :public_url, null: false
      t.string :dns_record_id
      t.string :owner_plugin
      t.string :owner_key
      t.string :purpose
      t.string :local_service_url, null: false
      t.string :status, null: false, default: "pending"
      t.text :last_error

      t.timestamps
    end

    add_index :hubs_stable_webhook_hostnames, :hubs_cloudflare_tunnel_id, name: "idx_hubs_stable_hostnames_on_tunnel_id"
    add_index :hubs_stable_webhook_hostnames, :hostname, unique: true
    add_index :hubs_stable_webhook_hostnames, [ :hub_id, :owner_plugin, :owner_key ], name: "idx_hubs_stable_hostnames_on_owner"
    add_index :hubs_stable_webhook_hostnames, :status
  end
end
