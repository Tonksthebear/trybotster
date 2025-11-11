class AddGithubAppFieldsToUsers < ActiveRecord::Migration[8.1]
  def change
    add_column :users, :github_app_token, :string
    add_column :users, :github_app_refresh_token, :string
    add_column :users, :github_app_token_expires_at, :datetime
    add_column :users, :github_app_installation_id, :string
    add_column :users, :github_app_permissions, :jsonb, default: {}

    # Add index for token expiration queries
    add_index :users, :github_app_token_expires_at
  end
end
