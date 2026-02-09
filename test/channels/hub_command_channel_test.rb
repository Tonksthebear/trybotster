# frozen_string_literal: true

require "test_helper"

class HubCommandChannelTest < ActionCable::Channel::TestCase
  tests HubCommandChannel

  setup do
    @user = users(:jason)
    @hub = hubs(:active_hub)
    @test_repo = "botster/trybotster"
    stub_connection current_user: @user
  end

  # === Subscription Tests ===

  test "subscribes with valid hub id and streams from correct channel" do
    subscribe hub_id: @hub.id

    assert subscription.confirmed?
    assert_has_stream "hub_command:#{@hub.id}"
  end

  test "subscribes with repo and streams from github events channel" do
    subscribe hub_id: @hub.id, repo: @test_repo

    assert subscription.confirmed?
    assert_has_stream "hub_command:#{@hub.id}"
    assert_has_stream "github_events:#{@test_repo}"
  end

  test "subscribes without repo and does not stream github events" do
    subscribe hub_id: @hub.id

    assert subscription.confirmed?
    assert_has_stream "hub_command:#{@hub.id}"
    assert_has_no_stream "github_events:#{@test_repo}"
  end

  test "rejects subscription without hub_id" do
    subscribe

    assert subscription.rejected?
  end

  test "rejects subscription with nonexistent hub_id" do
    subscribe hub_id: "nonexistent-hub-xyz"

    assert subscription.rejected?
  end

  # === Replay Tests ===

  test "replays unacked hub commands on subscribe" do
    cmd1 = HubCommand.create_for_hub!(@hub, event_type: "browser_wants_preview", payload: { browser_identity: "b1" })
    cmd2 = HubCommand.create_for_hub!(@hub, event_type: "browser_wants_preview", payload: { browser_identity: "b2" })
    cmd3 = HubCommand.create_for_hub!(@hub, event_type: "browser_wants_preview", payload: { browser_identity: "b3" })

    # Acknowledge the first so it should not be replayed
    cmd1.acknowledge!

    subscribe hub_id: @hub.id, start_from: 0

    assert subscription.confirmed?

    # Should have transmitted cmd2 and cmd3 (cmd1 is acknowledged)
    assert_equal 2, transmissions.size
    assert_equal cmd2.sequence, transmissions[0]["sequence"]
    assert_equal cmd3.sequence, transmissions[1]["sequence"]
  end

  test "replays only hub commands after start_from sequence" do
    cmd1 = HubCommand.create_for_hub!(@hub, event_type: "browser_wants_preview", payload: { browser_identity: "b1" })
    cmd2 = HubCommand.create_for_hub!(@hub, event_type: "browser_wants_preview", payload: { browser_identity: "b2" })
    cmd3 = HubCommand.create_for_hub!(@hub, event_type: "browser_wants_preview", payload: { browser_identity: "b3" })

    subscribe hub_id: @hub.id, start_from: cmd2.sequence

    assert subscription.confirmed?

    assert_equal 1, transmissions.size
    assert_equal cmd3.sequence, transmissions[0]["sequence"]
  end

  test "replays pending GitHub messages for repo on subscribe" do
    msg = Integrations::Github::Message.create!(
      event_type: "github_mention",
      repo: @test_repo,
      issue_number: 42,
      payload: { repo: @test_repo, issue_number: 42 }
    )

    subscribe hub_id: @hub.id, start_from: 0, repo: @test_repo

    assert subscription.confirmed?

    # Find the GitHub message in transmissions (sequence: -1)
    github_msgs = transmissions.select { |t| t["sequence"] == Integrations::Github::Message::NO_SEQUENCE }
    assert_equal 1, github_msgs.size
    assert_equal msg.id, github_msgs.first["id"]
    assert_equal "github_mention", github_msgs.first["event_type"]
  end

  # === Ack Action Tests ===

  test "ack action acknowledges a hub command" do
    cmd = HubCommand.create_for_hub!(@hub, event_type: "browser_wants_preview", payload: { browser_identity: "b1" })

    subscribe hub_id: @hub.id
    assert subscription.confirmed?

    perform :ack, sequence: cmd.sequence

    cmd.reload
    assert cmd.acknowledged?
    assert_equal "acknowledged", cmd.status
  end

  test "ack_github action acknowledges a GitHub message" do
    msg = Integrations::Github::Message.create!(
      event_type: "github_mention",
      repo: @test_repo,
      issue_number: 42,
      payload: { repo: @test_repo, issue_number: 42 }
    )

    subscribe hub_id: @hub.id, repo: @test_repo
    assert subscription.confirmed?

    # Stub the eyes reaction
    Github::App.stub :create_comment_reaction, { success: true } do
      Github::App.stub :create_issue_reaction, { success: true } do
        perform :ack_github, id: msg.id
      end
    end

    msg.reload
    assert msg.acknowledged?
  end

  # === Heartbeat Action Tests ===

  test "heartbeat action updates hub last_seen_at and alive" do
    @hub.update!(alive: false, last_seen_at: 10.minutes.ago)

    subscribe hub_id: @hub.id
    assert subscription.confirmed?

    perform :heartbeat, agents: []

    @hub.reload
    assert @hub.alive?
    assert @hub.last_seen_at > 1.minute.ago
  end
end
