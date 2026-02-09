# frozen_string_literal: true

class CreateGithubMessages < ActiveRecord::Migration[8.0]
  def change
    create_table :github_messages do |t|
      t.string :event_type, null: false
      t.string :repo, null: false
      t.integer :issue_number
      t.jsonb :payload, null: false, default: {}
      t.string :status, null: false, default: "pending"
      t.datetime :acknowledged_at

      t.timestamps
    end

    add_index :github_messages, :repo
    add_index :github_messages, :status
    add_index :github_messages, [:repo, :status]
    add_index :github_messages, :event_type
  end
end
