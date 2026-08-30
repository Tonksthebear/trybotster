class DropBrowserKeys < ActiveRecord::Migration[8.1]
  def change
    drop_table :browser_keys do |t|
      t.references :user, null: false, foreign_key: true
      t.string :name, null: false
      t.string :public_key, null: false
      t.string :fingerprint, null: false
      t.datetime :last_seen_at
      t.timestamps

      t.index :public_key, unique: true
      t.index [ :user_id, :fingerprint ], unique: true
    end
  end
end
