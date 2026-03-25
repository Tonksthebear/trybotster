# frozen_string_literal: true

class CollapseDeviceIntoHub < ActiveRecord::Migration[8.1]
  def up
    # 1. Create browser_keys table for browser E2E encryption key registrations
    create_table :browser_keys do |t|
      t.references :user, null: false, foreign_key: true
      t.string :name, null: false
      t.string :public_key, null: false
      t.string :fingerprint, null: false
      t.datetime :last_seen_at
      t.timestamps
      t.index :public_key, unique: true
      t.index [ :user_id, :fingerprint ], unique: true
    end

    # Migrate browser devices to browser_keys
    execute <<~SQL
      INSERT INTO browser_keys (user_id, name, public_key, fingerprint, last_seen_at, created_at, updated_at)
      SELECT user_id, name, public_key, fingerprint, last_seen_at, created_at, updated_at
      FROM devices WHERE device_type = 'browser'
    SQL

    # 2. Add device-identity columns to hubs
    add_column :hubs, :fingerprint, :string
    add_column :hubs, :notifications_enabled, :boolean, default: false, null: false
    add_index :hubs, :fingerprint

    # 3. Delete all existing hubs — we'll create fresh ones from CLI devices.
    #    Hub_commands depend on hubs via FK, so delete those first.
    execute "DELETE FROM hub_commands"
    execute "DELETE FROM hubs"

    # 4. Create one hub per CLI device, carrying over identity fields
    execute <<~SQL
      INSERT INTO hubs (user_id, identifier, name, fingerprint, notifications_enabled,
                        alive, last_seen_at, message_sequence, created_at, updated_at)
      SELECT user_id,
             fingerprint,
             name,
             fingerprint,
             notifications_enabled,
             false,
             COALESCE(last_seen_at, NOW()),
             0,
             created_at,
             updated_at
      FROM devices
      WHERE device_type = 'cli'
    SQL

    # 5. Rename device_tokens → hub_tokens, repoint to new hubs
    rename_table :device_tokens, :hub_tokens
    add_reference :hub_tokens, :hub, foreign_key: true

    # Point each token at the hub created from its device
    execute <<~SQL
      UPDATE hub_tokens SET hub_id = hubs.id
      FROM hubs, devices
      WHERE hub_tokens.device_id = devices.id
        AND hubs.fingerprint = devices.fingerprint
        AND hubs.user_id = devices.user_id
    SQL

    # Tokens that didn't match a hub (orphaned) — delete them
    execute "DELETE FROM hub_tokens WHERE hub_id IS NULL"

    change_column_null :hub_tokens, :hub_id, false
    remove_foreign_key :hub_tokens, column: :device_id
    remove_column :hub_tokens, :device_id

    # 6. Repoint MCP tokens to new hubs
    add_reference :integrations_github_mcp_tokens, :hub, foreign_key: true

    execute <<~SQL
      UPDATE integrations_github_mcp_tokens SET hub_id = hubs.id
      FROM hubs, devices
      WHERE integrations_github_mcp_tokens.device_id = devices.id
        AND hubs.fingerprint = devices.fingerprint
        AND hubs.user_id = devices.user_id
    SQL

    # Orphaned MCP tokens — delete them
    execute "DELETE FROM integrations_github_mcp_tokens WHERE hub_id IS NULL"

    change_column_null :integrations_github_mcp_tokens, :hub_id, false
    remove_foreign_key :integrations_github_mcp_tokens, column: :device_id
    remove_column :integrations_github_mcp_tokens, :device_id

    # 7. Rename device_authorizations → hub_authorizations
    rename_table :device_authorizations, :hub_authorizations

    # 8. Drop devices table
    remove_foreign_key :hubs, :devices
    remove_column :hubs, :device_id
    drop_table :devices
  end

  def down
    raise ActiveRecord::IrreversibleMigration
  end
end
