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

    # 2. Add columns to hubs
    add_column :hubs, :fingerprint, :string
    add_column :hubs, :notifications_enabled, :boolean, default: false, null: false
    add_index :hubs, :fingerprint

    # Migrate device data into hubs
    execute <<~SQL
      UPDATE hubs SET
        fingerprint = devices.fingerprint,
        notifications_enabled = devices.notifications_enabled,
        name = COALESCE(NULLIF(hubs.name, ''), devices.name)
      FROM devices
      WHERE hubs.device_id = devices.id
    SQL

    # 3. Rename device_tokens → hub_tokens
    rename_table :device_tokens, :hub_tokens

    # Add hub_id reference
    add_reference :hub_tokens, :hub, foreign_key: true

    # Populate hub_id from device_id via hubs table
    execute <<~SQL
      UPDATE hub_tokens SET hub_id = hubs.id
      FROM hubs
      WHERE hub_tokens.device_id = hubs.device_id
    SQL

    # Make hub_id NOT NULL
    change_column_null :hub_tokens, :hub_id, false

    # Remove device_id FK and column
    remove_foreign_key :hub_tokens, column: :device_id
    remove_column :hub_tokens, :device_id

    # 4. Repoint integrations_github_mcp_tokens
    add_reference :integrations_github_mcp_tokens, :hub, foreign_key: true

    # Populate hub_id from device_id via hubs table
    execute <<~SQL
      UPDATE integrations_github_mcp_tokens SET hub_id = hubs.id
      FROM hubs
      WHERE integrations_github_mcp_tokens.device_id = hubs.device_id
    SQL

    # Make hub_id NOT NULL
    change_column_null :integrations_github_mcp_tokens, :hub_id, false

    # Remove device_id FK and column
    remove_foreign_key :integrations_github_mcp_tokens, column: :device_id
    remove_column :integrations_github_mcp_tokens, :device_id

    # 5. Rename device_authorizations → hub_authorizations
    rename_table :device_authorizations, :hub_authorizations

    # 6. Drop devices
    remove_foreign_key :hubs, :devices
    remove_column :hubs, :device_id
    drop_table :devices
  end

  def down
    raise ActiveRecord::IrreversibleMigration
  end
end
