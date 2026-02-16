class RemoveServerAssistedPairingFromUsers < ActiveRecord::Migration[8.1]
  def change
    remove_column :users, :server_assisted_pairing, :boolean
  end
end
