# frozen_string_literal: true

require "test_helper"
require "minitest/mock"

class Integrations::Github::MessageTest < ActiveSupport::TestCase
  include ActionCable::TestHelper

  setup do
    @message_with_comment = Integrations::Github::Message.new(
      event_type: "github_mention",
      repo: "owner/repo",
      issue_number: 123,
      payload: {
        repo: "owner/repo",
        issue_number: 123,
        comment_id: 456789,
        installation_id: 12345
      }
    )

    @message_with_issue_only = Integrations::Github::Message.new(
      event_type: "github_mention",
      repo: "owner/repo",
      issue_number: 123,
      payload: {
        repo: "owner/repo",
        issue_number: 123,
        comment_id: nil,
        installation_id: 12345
      }
    )

    @message_without_installation = Integrations::Github::Message.new(
      event_type: "github_mention",
      repo: "owner/repo",
      issue_number: 123,
      payload: {
        repo: "owner/repo",
        issue_number: 123,
        comment_id: 456789,
        installation_id: nil
      }
    )
  end

  # Payload accessor tests
  test "extracts comment_id from payload" do
    assert_equal 456789, @message_with_comment.comment_id
  end

  test "extracts installation_id from payload" do
    assert_equal 12345, @message_with_comment.installation_id
  end

  # Reaction tests
  test "adds eyes reaction to comment when comment_id is present" do
    mock = Minitest::Mock.new
    mock.expect :call, { success: true, reaction: {} }, [ 12345 ], repo: "owner/repo", comment_id: 456789, reaction: "eyes"

    Github::App.stub :create_comment_reaction, mock do
      @message_with_comment.send(:add_eyes_reaction)
    end

    assert mock.verify, "Expected create_comment_reaction to be called with correct args"
  end

  test "adds eyes reaction to issue when comment_id is nil" do
    mock = Minitest::Mock.new
    mock.expect :call, { success: true, reaction: {} }, [ 12345 ], repo: "owner/repo", issue_number: 123, reaction: "eyes"

    Github::App.stub :create_issue_reaction, mock do
      @message_with_issue_only.send(:add_eyes_reaction)
    end

    assert mock.verify, "Expected create_issue_reaction to be called with correct args"
  end

  test "skips reaction when installation_id is missing" do
    @message_without_installation.send(:add_eyes_reaction)
    assert true
  end

  test "skips reaction for non-github_mention events" do
    message = Integrations::Github::Message.new(
      event_type: "agent_cleanup",
      repo: "owner/repo",
      payload: {
        repo: "owner/repo",
        issue_number: 123,
        installation_id: 12345
      }
    )

    message.send(:add_eyes_reaction)
    assert true
  end

  test "handles reaction API failure gracefully" do
    Github::App.stub :create_comment_reaction, { success: false, error: "API error" } do
      assert_nothing_raised do
        @message_with_comment.send(:add_eyes_reaction)
      end
    end
  end

  # Acknowledge tests
  test "acknowledge! updates status and adds reaction" do
    message = Integrations::Github::Message.create!(
      event_type: "github_mention",
      repo: "owner/repo",
      issue_number: 123,
      payload: {
        repo: "owner/repo",
        issue_number: 123,
        comment_id: 456789,
        installation_id: 12345
      }
    )

    Github::App.stub :create_comment_reaction, { success: true, reaction: {} } do
      message.acknowledge!
    end

    assert_equal "acknowledged", message.status
    assert_not_nil message.acknowledged_at
  end

  # Status tests
  test "default status is pending" do
    message = Integrations::Github::Message.create!(
      event_type: "github_mention",
      repo: "test/repo",
      issue_number: 1,
      payload: { repo: "test/repo", issue_number: 1 }
    )
    assert_equal "pending", message.status
  end

  # Broadcast tests
  test "broadcasts to repo stream on create" do
    assert_broadcasts("github_events:test/repo", 1) do
      Integrations::Github::Message.create!(
        event_type: "github_mention",
        repo: "test/repo",
        issue_number: 1,
        payload: { repo: "test/repo", issue_number: 1 }
      )
    end
  end

  # Scope tests
  test "for_repo scope filters by repo" do
    msg1 = Integrations::Github::Message.create!(
      event_type: "github_mention",
      repo: "owner/repo-a",
      payload: { repo: "owner/repo-a" }
    )
    msg2 = Integrations::Github::Message.create!(
      event_type: "github_mention",
      repo: "owner/repo-b",
      payload: { repo: "owner/repo-b" }
    )

    results = Integrations::Github::Message.for_repo("owner/repo-a")
    assert_includes results, msg1
    refute_includes results, msg2
  end

  # Validation tests
  test "validates event_type inclusion" do
    message = Integrations::Github::Message.new(
      event_type: "invalid_type",
      repo: "test/repo",
      payload: { foo: "bar" }
    )
    refute message.valid?
    assert_includes message.errors[:event_type], "invalid_type is not a valid event type"
  end

  test "validates repo presence" do
    message = Integrations::Github::Message.new(
      event_type: "github_mention",
      payload: { foo: "bar" }
    )
    refute message.valid?
    assert_includes message.errors[:repo], "can't be blank"
  end
end
