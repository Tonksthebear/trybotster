# frozen_string_literal: true

require "test_helper"

class MemoryTagTest < ActiveSupport::TestCase
  test "belongs to memory and tag" do
    assert_respond_to MemoryTag.new, :memory
    assert_respond_to MemoryTag.new, :tag
  end

  test "enforces uniqueness of memory_id scoped to tag_id" do
    tag = Tag.create!(name: "test-mt")
    user = users(:jason)
    memory = Memory.create!(user: user, content: "test memory for tag")

    MemoryTag.create!(memory: memory, tag: tag)
    duplicate = MemoryTag.new(memory: memory, tag: tag)
    assert_not duplicate.valid?
  ensure
    MemoryTag.where(memory: memory).destroy_all if memory&.persisted?
    memory&.delete
    tag&.destroy
  end
end
