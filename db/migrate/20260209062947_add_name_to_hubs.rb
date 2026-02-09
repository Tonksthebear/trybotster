class AddNameToHubs < ActiveRecord::Migration[8.1]
  def change
    add_column :hubs, :name, :string
  end
end
