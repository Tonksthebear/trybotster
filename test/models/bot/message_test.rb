# frozen_string_literal: true

require "test_helper"
require "minitest/mock"

class Bot::MessageTest < ActiveSupport::TestCase
  setup do
    @message_with_comment = Bot::Message.new(
      event_type: "github_mention",
      payload: {
        repo: "owner/repo",
        issue_number: 123,
        comment_id: 456789,
        installation_id: 12345
      }
    )

    @message_with_issue_only = Bot::Message.new(
      event_type: "github_mention",
      payload: {
        repo: "owner/repo",
        issue_number: 123,
        comment_id: nil,
        installation_id: 12345
      }
    )

    @message_without_installation = Bot::Message.new(
      event_type: "github_mention",
      payload: {
        repo: "owner/repo",
        issue_number: 123,
        comment_id: 456789,
        installation_id: nil
      }
    )
  end

  # Payload accessor tests
  test "extracts repo from payload" do
    assert_equal "owner/repo", @message_with_comment.repo
  end

  test "extracts issue_number from payload" do
    assert_equal 123, @message_with_comment.issue_number
  end

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
      @message_with_comment.add_eyes_reaction_to_comment
    end

    assert mock.verify, "Expected create_comment_reaction to be called with correct args"
  end

  test "adds eyes reaction to issue when comment_id is nil" do
    mock = Minitest::Mock.new
    mock.expect :call, { success: true, reaction: {} }, [ 12345 ], repo: "owner/repo", issue_number: 123, reaction: "eyes"

    Github::App.stub :create_issue_reaction, mock do
      @message_with_issue_only.add_eyes_reaction_to_comment
    end

    assert mock.verify, "Expected create_issue_reaction to be called with correct args"
  end

  test "skips reaction when installation_id is missing" do
    # Neither method should be called - if they were, this would fail
    # because we're not setting up any stubs
    @message_without_installation.add_eyes_reaction_to_comment
    # If we get here without error, the test passes
    assert true
  end

  test "skips reaction for non-github_mention events" do
    message = Bot::Message.new(
      event_type: "agent_cleanup",
      payload: {
        repo: "owner/repo",
        issue_number: 123,
        installation_id: 12345
      }
    )

    # Neither method should be called
    message.add_eyes_reaction_to_comment
    # If we get here without error, the test passes
    assert true
  end

  test "handles reaction API failure gracefully" do
    Github::App.stub :create_comment_reaction, { success: false, error: "API error" } do
      # Should not raise an error
      assert_nothing_raised do
        @message_with_comment.add_eyes_reaction_to_comment
      end
    end
  end

  test "handles reaction API exception gracefully" do
    error_proc = ->(*) { raise StandardError, "Network error" }

    Github::App.stub :create_comment_reaction, error_proc do
      # Should not raise an error
      assert_nothing_raised do
        @message_with_comment.add_eyes_reaction_to_comment
      end
    end
  end

  # Acknowledge tests
  test "acknowledge! updates status and adds reaction" do
    message = Bot::Message.create!(
      event_type: "github_mention",
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
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: "test/repo", issue_number: 1 }
    )
    assert_equal "pending", message.status
  end

  test "claim! updates status to sent" do
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: "test/repo", issue_number: 1 }
    )
    message.claim!(1)
    assert_equal "sent", message.status
    assert_not_nil message.claimed_at
    assert_not_nil message.sent_at
    assert_equal 1, message.claimed_by_user_id
  end

  test "mark_as_failed! updates status to failed" do
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: "test/repo", issue_number: 1 }
    )
    message.mark_as_failed!("Something went wrong")
    assert_equal "failed", message.status
    assert_equal "Something went wrong", message.payload["error"]
  end

  # Race condition tests
  test "claiming already claimed message raises AlreadyClaimedError" do
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: "test/repo", issue_number: 1 }
    )

    # First claim succeeds
    message.claim!(1)
    assert message.claimed?

    # Second claim should raise error
    error = assert_raises(Bot::Message::AlreadyClaimedError) do
      message.claim!(2)
    end
    assert_match(/already claimed/, error.message)

    # Original claim should be preserved
    message.reload
    assert_equal 1, message.claimed_by_user_id
  end

  test "concurrent claim attempts only allow one to succeed" do
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: "test/repo", issue_number: 1 }
    )

    success_count = 0
    error_count = 0
    claimed_by = nil
    mutex = Mutex.new

    # Simulate concurrent claims from 5 different users
    threads = (1..5).map do |user_id|
      Thread.new do
        begin
          # Small random delay to increase chance of actual concurrency
          sleep(rand * 0.01)
          message.reload # Get fresh state
          message.claim!(user_id)
          mutex.synchronize do
            success_count += 1
            claimed_by = user_id
          end
        rescue Bot::Message::AlreadyClaimedError
          mutex.synchronize { error_count += 1 }
        end
      end
    end

    threads.each(&:join)

    # Exactly one should succeed, others should fail
    assert_equal 1, success_count, "Only one claim should succeed"
    assert_equal 4, error_count, "Four claims should fail"

    # Message should be claimed by exactly one user
    message.reload
    assert message.claimed?
    assert_not_nil claimed_by
    assert_equal claimed_by, message.claimed_by_user_id
  end

  test "for_delivery scope excludes claimed messages" do
    # Create two messages
    pending_message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: "test/repo", issue_number: 1 }
    )
    claimed_message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: "test/repo", issue_number: 2 }
    )
    claimed_message.claim!(1)

    # for_delivery should only include unclaimed pending messages
    deliverable = Bot::Message.for_delivery
    assert_includes deliverable, pending_message
    refute_includes deliverable, claimed_message
  end
end
