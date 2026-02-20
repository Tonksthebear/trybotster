class AddFingerprintToDeviceAuthorizations < ActiveRecord::Migration[8.1]
  def change
    add_column :device_authorizations, :fingerprint, :string
  end
end
