class Tag < ApplicationRecord
  has_many :memory_tags, dependent: :destroy
  has_many :memories, through: :memory_tags

  validates :name, presence: true, uniqueness: { case_sensitive: false }

  # Optional: Normalize name (e.g., downcase, strip)
  before_save :normalize_name

  private

  def normalize_name
    self.name = name.downcase.strip if name.present?
  end
end
