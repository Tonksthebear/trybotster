class RemoveVectorEmbeddingFromMemories < ActiveRecord::Migration[8.1]
  def up
    remove_column :memories, :embedding, if_exists: true
    disable_extension "vector" if extension_enabled?("vector")
  end

  def down
    enable_extension "vector"
    add_column :memories, :embedding, :vector, limit: 1536
  end
end
