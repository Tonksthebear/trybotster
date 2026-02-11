class AddAliveToHubs < ActiveRecord::Migration[8.1]
  def change
    add_column :hubs, :alive, :boolean, default: false, null: false
  end
end
