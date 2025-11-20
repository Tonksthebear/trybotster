class RemoveGithubAppInstallationIdFromUsers < ActiveRecord::Migration[8.1]
  def change
    remove_column :users, :github_app_installation_id, :string
  end
end
