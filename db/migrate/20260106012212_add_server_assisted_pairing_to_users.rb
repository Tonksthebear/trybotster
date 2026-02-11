class AddServerAssistedPairingToUsers < ActiveRecord::Migration[8.1]
  def change
    # Default to FALSE - secure mode is the default
    # Users must explicitly opt-in to server-assisted pairing
    # which allows connecting without scanning QR codes but is less secure (potential MITM)
    add_column :users, :server_assisted_pairing, :boolean, default: false, null: false
  end
end
