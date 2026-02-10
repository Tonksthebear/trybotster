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

    # Skip after_save vectorsearch callback to avoid hitting external embedding API
    memory = Memory.new(user: user, content: "test memory for tag")
    Memory.skip_callback(:save, :after, :upsert_to_vectorsearch)
    memory.save!
    Memory.set_callback(:save, :after, :upsert_to_vectorsearch)

    MemoryTag.create!(memory: memory, tag: tag)
    duplicate = MemoryTag.new(memory: memory, tag: tag)
    assert_not duplicate.valid?
  ensure
    Memory.set_callback(:save, :after, :upsert_to_vectorsearch)
    MemoryTag.where(memory: memory).destroy_all if memory&.persisted?
    memory&.delete
    tag&.destroy
  end
end
