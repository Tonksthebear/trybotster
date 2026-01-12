# frozen_string_literal: true

require_relative "cli_integration_test_case"

# Tests CLI message polling against real Rails server.
#
# Verifies the CLI correctly:
# - Polls for pending messages (GET /hubs/:identifier/messages)
# - Claims messages during polling (status pending → sent)
# - Acknowledges processed messages (PATCH /hubs/:identifier/messages/:id)
# - Updates heartbeat timestamps
#
# These tests spawn real CLI binary and verify database state changes.
#
# Note: GitHub repo access checks are bypassed in test environment
# (see User#has_github_repo_access?) to allow testing with fake repos.
#
class CliMessagePollingTest < CliIntegrationTestCase
  test "CLI claims pending messages when polling" do
    # Create a pending message for this hub's repo
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: {
        repo: @hub.repo,
        issue_number: 42,
        comment_body: "Test message for polling"
      },
      status: "pending"
    )

    assert_equal "pending", message.status, "Message should start as pending"

    # Start CLI - it will poll and claim the message
    cli = start_cli(@hub, timeout: 20)

    # Wait for CLI to poll and claim the message
    # The polling interval is short in test mode
    assert_message_claimed(message, timeout: 10)

    message.reload
    assert_includes ["sent", "acknowledged"], message.status, "Message should be claimed (sent or acknowledged) after CLI polls"
    assert_equal @user.id, message.claimed_by_user_id, "Message should be claimed by correct user"
    assert message.claimed_at.present?, "claimed_at should be set"
  end

  test "CLI acknowledges messages after claiming" do
    # Create a message - use agent_cleanup type which doesn't spawn Claude
    # This allows us to test the full claim → acknowledge flow
    message = Bot::Message.create!(
      event_type: "agent_cleanup",
      payload: {
        repo: @hub.repo,
        issue_number: 99999,  # Required by CLI's message_to_hub_action
        cleanup_reason: "test"
      },
      status: "pending"
    )

    cli = start_cli(@hub, timeout: 20)

    # Wait for acknowledgment (status → acknowledged)
    assert_message_acknowledged(message, timeout: 15)

    message.reload
    assert_equal "acknowledged", message.status
    assert message.acknowledged_at.present?, "acknowledged_at should be set"
  end

  test "CLI does not claim messages for other repos" do
    # Create message for a different repo
    other_message = Bot::Message.create!(
      event_type: "github_mention",
      payload: {
        repo: "other/repo",
        issue_number: 99,
        comment_body: "Should not be claimed"
      },
      status: "pending"
    )

    # Create message for this hub's repo (control)
    our_message = Bot::Message.create!(
      event_type: "github_mention",
      payload: {
        repo: @hub.repo,
        issue_number: 42,
        comment_body: "Should be claimed"
      },
      status: "pending"
    )

    cli = start_cli(@hub, timeout: 20)

    # Wait for our message to be claimed
    assert_message_claimed(our_message, timeout: 10)

    # Other repo's message should still be pending
    other_message.reload
    assert_equal "pending", other_message.status, "Message for other repo should remain pending"
    assert_nil other_message.claimed_by_user_id

    # Cleanup
    other_message.destroy
  end

  test "CLI updates hub last_seen_at during polling" do
    # Set hub's last_seen_at to a known old time
    old_time = 5.minutes.ago
    @hub.update!(last_seen_at: old_time)

    cli = start_cli(@hub, timeout: 20)

    # Wait a moment for CLI to send heartbeat
    sleep 2

    @hub.reload
    assert @hub.last_seen_at > old_time, "Hub last_seen_at should be updated by CLI"
    assert @hub.last_seen_at > 30.seconds.ago, "Hub should have been seen recently"
  end

  test "CLI processes multiple messages in sequence" do
    # Create multiple messages
    messages = 3.times.map do |i|
      Bot::Message.create!(
        event_type: "agent_cleanup",
        payload: {
          repo: @hub.repo,
          session_key: "session-#{i}",
          cleanup_reason: "test"
        },
        status: "pending"
      )
    end

    cli = start_cli(@hub, timeout: 20)

    # Wait for all messages to be claimed
    messages.each do |msg|
      assert_message_claimed(msg, timeout: 15)
    end

    # All should be claimed by the same user
    messages.each(&:reload)
    messages.each do |msg|
      assert_equal @user.id, msg.claimed_by_user_id
    end
  end

  test "CLI does not reclaim already claimed messages" do
    # Create a message already claimed by another user
    other_user = users(:one)
    claimed_message = Bot::Message.create!(
      event_type: "github_mention",
      payload: {
        repo: @hub.repo,
        issue_number: 42,
        comment_body: "Already claimed"
      },
      status: "sent",
      claimed_at: 1.minute.ago,
      claimed_by_user_id: other_user.id
    )

    cli = start_cli(@hub, timeout: 20)

    # Wait a bit for any polling to happen
    sleep 3

    # Message should still be claimed by original user
    claimed_message.reload
    assert_equal other_user.id, claimed_message.claimed_by_user_id, "Message should remain claimed by original user"
  end

  private

  def assert_message_claimed(message, timeout: 10)
    deadline = Time.current + timeout
    while Time.current < deadline
      message.reload
      return true if message.status == "sent" || message.status == "acknowledged"
      sleep 0.5
    end
    flunk "Message #{message.id} was not claimed within #{timeout}s (status: #{message.status})"
  end

  def assert_message_acknowledged(message, timeout: 15)
    deadline = Time.current + timeout
    while Time.current < deadline
      message.reload
      return true if message.status == "acknowledged"
      sleep 0.5
    end
    flunk "Message #{message.id} was not acknowledged within #{timeout}s (status: #{message.status})"
  end
end
