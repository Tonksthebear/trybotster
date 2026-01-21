class ChangeDeviceTokenToDevice < ActiveRecord::Migration[8.1]
  def change
    # Clear existing tokens (no legacy support needed)
    execute "DELETE FROM device_tokens"

    # Remove user_id and add device_id
    remove_reference :device_tokens, :user, foreign_key: true
    add_reference :device_tokens, :device, null: false, foreign_key: true
  end
end
