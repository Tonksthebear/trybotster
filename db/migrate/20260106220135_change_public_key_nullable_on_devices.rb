class ChangePublicKeyNullableOnDevices < ActiveRecord::Migration[8.1]
  def change
    # Allow public_key to be null for CLI devices in secure mode
    # In secure mode, key exchange happens via QR code URL fragment, not server
    change_column_null :devices, :public_key, true

    # Update unique index to only apply to non-null values
    # (PostgreSQL already treats NULLs as distinct, but this makes intent clear)
    remove_index :devices, :public_key
    add_index :devices, :public_key, unique: true, where: "public_key IS NOT NULL"
  end
end
