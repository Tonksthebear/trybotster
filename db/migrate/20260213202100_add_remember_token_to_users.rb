class AddRememberTokenToUsers < ActiveRecord::Migration[8.1]
  def change
    add_column :users, :remember_token, :string
  end
end
