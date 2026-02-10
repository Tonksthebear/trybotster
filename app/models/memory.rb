class Memory < ApplicationRecord
  belongs_to :user
  belongs_to :team, optional: true
  belongs_to :parent, class_name: "Memory", optional: true
  has_many :children, class_name: "Memory", foreign_key: :parent_id, dependent: :nullify
  has_many :memory_tags, dependent: :destroy
  has_many :tags, through: :memory_tags

  enum :memory_type, { fact: "fact", insight: "insight", code_snippet: "code_snippet", summary: "summary", other: "other" }, default: :other
  enum :visibility, { private: "private", team: "team", public: "public" }, default: :private, prefix: true

  validates :content, presence: true
  validates :visibility, presence: true

  attribute :metadata, :json, default: {}

  scope :accessible_by, ->(user) {
    where(user: user).or(where(visibility: "public")).or(where(visibility: "team", team: user.team))
  }
end
