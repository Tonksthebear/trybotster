class AddVectorColumnToMemories < ActiveRecord::Migration[8.1]
  def change
    add_column :memories, :embedding, :vector,
      limit: LangchainrbRails
        .config
        .vectorsearch
        .llm
        .default_dimensions
  end
end
