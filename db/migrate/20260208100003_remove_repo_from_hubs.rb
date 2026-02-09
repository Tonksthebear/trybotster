# frozen_string_literal: true

class RemoveRepoFromHubs < ActiveRecord::Migration[8.1]
  def change
    remove_index :hubs, [ :repo, :last_seen_at ]
    remove_column :hubs, :repo, :string, null: false
  end
end
