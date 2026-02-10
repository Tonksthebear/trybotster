# frozen_string_literal: true

require "test_helper"

class TagTest < ActiveSupport::TestCase
  test "valid tag" do
    tag = Tag.new(name: "ruby")
    assert tag.valid?
  end

  test "requires name" do
    tag = Tag.new(name: nil)
    assert_not tag.valid?
    assert_includes tag.errors[:name], "can't be blank"
  end

  test "name must be unique (case insensitive)" do
    Tag.create!(name: "ruby")
    duplicate = Tag.new(name: "Ruby")
    assert_not duplicate.valid?
  ensure
    Tag.where(name: "ruby").destroy_all
  end

  test "normalizes name to lowercase and stripped" do
    tag = Tag.create!(name: "  Ruby  ")
    assert_equal "ruby", tag.name
  ensure
    tag&.destroy
  end

  test "has many memories through memory_tags" do
    assert_respond_to Tag.new, :memories
    assert_respond_to Tag.new, :memory_tags
  end
end
