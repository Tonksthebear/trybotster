class CreateDevices < ActiveRecord::Migration[8.1]
  def change
    create_table :devices do |t|
      t.references :user, null: false, foreign_key: true
      t.string :public_key, null: false
      t.string :device_type, null: false  # 'cli' or 'browser'
      t.string :name, null: false
      t.string :fingerprint, null: false  # Short hash for visual verification
      t.datetime :last_seen_at

      t.timestamps
    end

    add_index :devices, :public_key, unique: true
    add_index :devices, [ :user_id, :device_type ]
    add_index :devices, :fingerprint
  end
end
