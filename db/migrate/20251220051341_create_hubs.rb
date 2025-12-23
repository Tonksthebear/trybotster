class CreateHubs < ActiveRecord::Migration[8.1]
  def change
    create_table :hubs do |t|
      t.references :user, null: false, foreign_key: true
      t.string :repo, null: false
      t.string :identifier, null: false
      t.datetime :last_seen_at, null: false

      t.timestamps
    end
    add_index :hubs, :identifier, unique: true
    add_index :hubs, [:repo, :last_seen_at]
  end
end
