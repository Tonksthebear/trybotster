class CreateMemories < ActiveRecord::Migration[7.1]
  def change
    create_table :memories do |t|
      t.references :user, null: false, foreign_key: true
      t.references :team, null: true, foreign_key: true
      t.text :content, null: false
      t.jsonb :metadata, default: {}
      t.string :memory_type, default: 'other'
      t.string :source
      t.references :parent, null: true, foreign_key: { to_table: :memories }
      t.string :visibility, null: false, default: 'private'

      t.timestamps
    end

    # Indexes for performance
    add_index :memories, :metadata, using: :gin  # For JSONB queries
    add_index :memories, :memory_type
    add_index :memories, :source
    add_index :memories, :visibility
    add_index :memories, :created_at  # For sorting by recency
  end
end
