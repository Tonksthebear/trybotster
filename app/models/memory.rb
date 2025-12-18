class Memory < ApplicationRecord
  vectorsearch

  after_save :upsert_to_vectorsearch

  belongs_to :user
  belongs_to :team, optional: true
  belongs_to :parent, class_name: "Memory", optional: true
  has_many :children, class_name: "Memory", foreign_key: :parent_id, dependent: :nullify  # For hierarchies
  has_many :memory_tags, dependent: :destroy
  has_many :tags, through: :memory_tags

  enum :memory_type, { fact: "fact", insight: "insight", code_snippet: "code_snippet", summary: "summary", other: "other" }, default: :other
  enum :visibility, { private: "private", team: "team", public: "public" }, default: :private, prefix: true

  validates :content, presence: true
  validates :visibility, presence: true

  attribute :metadata, :json, default: {}

  # Scope for access control (e.g., in controllers)
  scope :accessible_by, ->(user) {
    where(user: user).or(where(visibility: "public")).or(where(visibility: "team", team: user.team))
  }

  # Semantic search (as before, but with filters)
  def self.similar_to(query, user, options = {})
    limit = options[:limit] || 5
    threshold = options[:threshold] || 0.7
    memory_type = options[:memory_type]
    visibility = options[:visibility] || %w[private team public]  # Array for filtering
    tag_names = options[:tags]  # e.g., ["ruby", "ai"]

    scope = accessible_by(user)
    scope = scope.joins(:tags).where(tags: { name: tag_names }) if tag_names.present?
    scope = scope.where(memory_type: memory_type) if memory_type
    scope = scope.where(visibility: visibility) if visibility.present?

    query_embedding = LangchainrbRails.llm.embed(content: query).embedding

    results = scope
      .order(Arel.sql("embedding <=> '#{query_embedding.to_json}'"))
      .limit(limit * 2)  # Overfetch for post-filtering

    # Filter by threshold and add hybrid (e.g., metadata tags)
    scored_results = results.select do |memory|
      score = similarity_score(memory.embedding, query_embedding)
      tag_match_boost = memory.metadata["tags"]&.any? { |tag| query.downcase.include?(tag.downcase) } ? 0.1 : 0
      score + tag_match_boost > threshold
    end

    scored_results.first(limit)
  end
end
