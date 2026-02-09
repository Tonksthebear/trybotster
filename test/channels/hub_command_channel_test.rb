# frozen_string_literal: true

require "test_helper"

class HubCommandChannelTest < ActionCable::Channel::TestCase
  tests HubCommandChannel

  setup do
    @user = users(:jason)
    @hub = hubs(:active_hub)
    stub_connection current_user: @user
  end

  # === Subscription Tests ===

  test "subscribes with valid hub id and streams from correct channel" do
    subscribe hub_id: @hub.id

    assert subscription.confirmed?
    assert_has_stream "hub_command:#{@hub.id}"
  end

  test "does not stream github events (handled by Github::EventsChannel)" do
    subscribe hub_id: @hub.id

    assert subscription.confirmed?
    assert_has_stream "hub_command:#{@hub.id}"
    assert_has_no_stream "github_events:botster/trybotster"
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
