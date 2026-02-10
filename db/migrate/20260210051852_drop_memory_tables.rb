class DropMemoryTables < ActiveRecord::Migration[8.1]
  def up
    drop_table :memory_tags, if_exists: true
    drop_table :memories, if_exists: true
    drop_table :tags, if_exists: true
  end

  def down
    create_table :memories do |t|
      t.references :user, null: false, foreign_key: true
      t.references :team, foreign_key: true
      t.references :parent, foreign_key: { to_table: :memories }
      t.text :content, null: false
      t.string :memory_type, default: "other"
      t.string :visibility, default: "private"
      t.string :source
      t.jsonb :metadata, default: {}
      t.timestamps
    end

    create_table :tags do |t|
      t.string :name, null: false
      t.timestamps
    end
    add_index :tags, :name, unique: true

    create_table :memory_tags do |t|
      t.references :memory, null: false, foreign_key: true
      t.references :tag, null: false, foreign_key: true
      t.timestamps
    end
    add_index :memory_tags, [:memory_id, :tag_id], unique: true
  end
end
