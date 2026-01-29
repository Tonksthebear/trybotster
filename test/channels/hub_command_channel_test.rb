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

  test "rejects subscription without hub_id" do
    subscribe

    assert subscription.rejected?
  end

  test "rejects subscription with nonexistent hub_id" do
    subscribe hub_id: "nonexistent-hub-xyz"

    assert subscription.rejected?
  end

  # === Replay Tests ===

  test "replays unacked messages on subscribe" do
    # Create messages with sequences for the hub
    msg1 = Bot::Message.create_for_hub!(@hub, event_type: "browser_connected", payload: { browser_identity: "b1" })
    msg2 = Bot::Message.create_for_hub!(@hub, event_type: "browser_connected", payload: { browser_identity: "b2" })
    msg3 = Bot::Message.create_for_hub!(@hub, event_type: "browser_disconnected", payload: { browser_identity: "b3" })

    # Acknowledge the first message so it should not be replayed
    msg1.acknowledge!

    subscribe hub_id: @hub.id, start_from: 0

    assert subscription.confirmed?

    # Should have transmitted msg2 and msg3 (msg1 is acknowledged)
    assert_equal 2, transmissions.size
    assert_equal msg2.sequence, transmissions[0]["sequence"]
    assert_equal msg3.sequence, transmissions[1]["sequence"]
  end

  test "replays only messages after start_from sequence" do
    msg1 = Bot::Message.create_for_hub!(@hub, event_type: "browser_connected", payload: { browser_identity: "b1" })
    msg2 = Bot::Message.create_for_hub!(@hub, event_type: "browser_connected", payload: { browser_identity: "b2" })
    msg3 = Bot::Message.create_for_hub!(@hub, event_type: "browser_disconnected", payload: { browser_identity: "b3" })

    # Subscribe with start_from at msg2's sequence, so only msg3 should replay
    subscribe hub_id: @hub.id, start_from: msg2.sequence

    assert subscription.confirmed?

    assert_equal 1, transmissions.size
    assert_equal msg3.sequence, transmissions[0]["sequence"]
  end

  # === Ack Action Tests ===

  test "ack action acknowledges a message" do
    msg = Bot::Message.create_for_hub!(@hub, event_type: "browser_connected", payload: { browser_identity: "b1" })

    subscribe hub_id: @hub.id
    assert subscription.confirmed?

    perform :ack, sequence: msg.sequence

    msg.reload
    assert msg.acknowledged?
    assert_equal "acknowledged", msg.status
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
