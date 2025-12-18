# frozen_string_literal: true

require "test_helper"

class IdempotencyKeyTest < ActiveSupport::TestCase
  test "creates idempotency key with required attributes" do
    key = IdempotencyKey.create!(
      key: "test-key-123",
      request_path: "/mcp/tools/github_comment_issue",
      request_params: { repo: "owner/repo", issue_number: 1, body: "test" }.to_json
    )

    assert key.persisted?
    assert_equal "test-key-123", key.key
    assert_equal "/mcp/tools/github_comment_issue", key.request_path
    assert key.request_params.present?
  end

  test "validates uniqueness of key" do
    IdempotencyKey.create!(
      key: "unique-key",
      request_path: "/mcp/tools/github_comment_issue"
    )

    duplicate = IdempotencyKey.new(
      key: "unique-key",
      request_path: "/mcp/tools/github_comment_issue"
    )

    assert_not duplicate.valid?
    assert_includes duplicate.errors[:key], "has already been taken"
  end

  test "validates presence of key" do
    idempotency_key = IdempotencyKey.new(request_path: "/some/path")
    assert_not idempotency_key.valid?
    assert_includes idempotency_key.errors[:key], "can't be blank"
  end

  test "validates presence of request_path" do
    idempotency_key = IdempotencyKey.new(key: "some-key")
    assert_not idempotency_key.valid?
    assert_includes idempotency_key.errors[:request_path], "can't be blank"
  end

  test "stores response data after successful execution" do
    key = IdempotencyKey.create!(
      key: "test-key-456",
      request_path: "/mcp/tools/github_comment_issue"
    )

    key.update!(
      response_body: { success: true, comment_url: "https://github.com/owner/repo/issues/1#comment-123" }.to_json,
      response_status: 200,
      completed_at: Time.current
    )

    assert key.completed?
    assert_equal 200, key.response_status
    assert key.response_body.present?
  end

  test "completed? returns true when completed_at is set" do
    key = IdempotencyKey.create!(
      key: "completed-key",
      request_path: "/mcp/tools/github_comment_issue",
      completed_at: Time.current
    )

    assert key.completed?
  end

  test "completed? returns false when completed_at is nil" do
    key = IdempotencyKey.create!(
      key: "pending-key",
      request_path: "/mcp/tools/github_comment_issue"
    )

    assert_not key.completed?
  end

  test "expired? returns true for keys older than retention period" do
    key = IdempotencyKey.create!(
      key: "old-key",
      request_path: "/mcp/tools/github_comment_issue"
    )
    key.update_column(:created_at, 25.hours.ago)

    assert key.expired?
  end

  test "expired? returns false for recent keys" do
    key = IdempotencyKey.create!(
      key: "recent-key",
      request_path: "/mcp/tools/github_comment_issue"
    )

    assert_not key.expired?
  end

  test "cleanup_expired removes old keys" do
    # Create an old key
    old_key = IdempotencyKey.create!(
      key: "old-cleanup-key",
      request_path: "/mcp/tools/github_comment_issue"
    )
    old_key.update_column(:created_at, 25.hours.ago)

    # Create a recent key
    recent_key = IdempotencyKey.create!(
      key: "recent-cleanup-key",
      request_path: "/mcp/tools/github_comment_issue"
    )

    IdempotencyKey.cleanup_expired

    assert_not IdempotencyKey.exists?(old_key.id)
    assert IdempotencyKey.exists?(recent_key.id)
  end

  test "find_or_create_for_request finds existing key" do
    existing = IdempotencyKey.create!(
      key: "existing-key",
      request_path: "/mcp/tools/github_comment_issue"
    )

    found = IdempotencyKey.find_or_create_for_request("existing-key", "/mcp/tools/github_comment_issue")

    assert_equal existing.id, found.id
  end

  test "find_or_create_for_request creates new key when not found" do
    key = IdempotencyKey.find_or_create_for_request("new-key-abc", "/mcp/tools/github_comment_issue", { repo: "owner/repo" })

    assert key.persisted?
    assert_equal "new-key-abc", key.key
  end

  test "mark_completed! updates completed_at and response data" do
    key = IdempotencyKey.create!(
      key: "to-complete-key",
      request_path: "/mcp/tools/github_comment_issue"
    )

    key.mark_completed!(
      status: 200,
      body: { success: true }.to_json
    )

    key.reload
    assert key.completed?
    assert_equal 200, key.response_status
    assert_equal({ "success" => true }, JSON.parse(key.response_body))
  end
end
