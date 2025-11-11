class MemorySerializer < ActiveModel::Serializer
  attributes :id, :content, :metadata, :memory_type, :source, :visibility, :created_at
  has_many :tags, serializer: TagSerializer  # Include tags in response

  # app/serializers/tag_serializer.rb
  class TagSerializer < ActiveModel::Serializer
    attributes :id, :name
  end
end
