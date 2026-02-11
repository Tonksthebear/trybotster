class CreateDeviceAuthorizations < ActiveRecord::Migration[8.1]
  def change
    create_table :device_authorizations do |t|
      t.string :device_code, null: false
      t.string :user_code, null: false
      t.references :user, foreign_key: true
      t.datetime :expires_at, null: false
      t.string :status, default: "pending", null: false
      t.string :device_name
      t.timestamps

      t.index :device_code, unique: true
      t.index :user_code, unique: true
    end
  end
end
