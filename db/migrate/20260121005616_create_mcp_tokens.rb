class CreateMCPTokens < ActiveRecord::Migration[8.1]
  def change
    create_table :mcp_tokens do |t|
      t.references :device, null: false, foreign_key: true
      t.string :token
      t.string :name
      t.datetime :last_used_at
      t.string :last_ip

      t.timestamps
    end
    add_index :mcp_tokens, :token, unique: true
  end
end
