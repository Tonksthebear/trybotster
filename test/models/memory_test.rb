# frozen_string_literal: true

require "test_helper"

class MemoryTest < ActiveSupport::TestCase
  setup do
    @user = users(:jason)
  end

  test "requires content" do
    memory = Memory.new(user: @user, content: nil)
    assert_not memory.valid?
    assert_includes memory.errors[:content], "can't be blank"
  end

  test "requires visibility" do
    memory = Memory.new(user: @user, content: "test", visibility: nil)
    assert_not memory.valid?
  end

  test "defaults visibility to private" do
    memory = Memory.new(user: @user, content: "test")
    assert_equal "private", memory.visibility
  end

  test "defaults memory_type to other" do
    memory = Memory.new(user: @user, content: "test")
    assert_equal "other", memory.memory_type
  end

  test "memory_type enum values" do
    assert_equal %w[fact insight code_snippet summary other], Memory.memory_types.keys
  end

  test "visibility enum values" do
    assert_equal %w[private team public], Memory.visibilities.keys
  end

  test "defaults metadata to empty hash" do
    memory = Memory.new(user: @user, content: "test")
    assert_equal({}, memory.metadata)
  end

  test "belongs to user" do
    assert_respond_to Memory.new, :user
  end

  test "has many tags through memory_tags" do
    assert_respond_to Memory.new, :tags
    assert_respond_to Memory.new, :memory_tags
  end

  test "parent/children hierarchy" do
    assert_respond_to Memory.new, :parent
    assert_respond_to Memory.new, :children
  end
end
