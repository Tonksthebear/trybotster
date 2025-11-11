class MemoryTag < ApplicationRecord
  belongs_to :memory
  belongs_to :tag

  validates :memory_id, uniqueness: { scope: :tag_id }  # No duplicate tags per memory
end
