class CleanupDeprecatedTablesAndColumns < ActiveRecord::Migration[8.1]
  def up
    # Drop orphaned webrtc_sessions table (no model, never used in production)
    drop_table :webrtc_sessions

    # Remove deprecated Tailscale columns from hubs (replaced by Signal Protocol)
    remove_column :hubs, :tailscale_hostname, :string
    remove_column :hubs, :tailscale_preauth_key, :string
  end

  def down
    # Recreate webrtc_sessions table (for rollback safety)
    create_table :webrtc_sessions do |t|
      t.references :user, null: false, foreign_key: true
      t.jsonb :offer, null: false
      t.jsonb :answer
      t.string :status, default: "pending", null: false
      t.datetime :expires_at, null: false
      t.timestamps

      t.index :user_id
      t.index :status
      t.index :expires_at
    end

    # Re-add Tailscale columns (for rollback safety)
    add_column :hubs, :tailscale_hostname, :string
    add_column :hubs, :tailscale_preauth_key, :string
  end
end
