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

  test "subscribes with valid hub id and streams from hub_command channel" do
    subscribe hub_id: @hub.id

    assert subscription.confirmed?
    assert_has_stream "hub_command:#{@hub.id}"
  end

  test "rejects subscription without hub_id" do
    subscribe

    assert subscription.rejected?
  end

  test "rejects subscription with nonexistent hub_id" do
    subscribe hub_id: 999_999

    assert subscription.rejected?
  end

  test "rejects subscription for hub owned by different user" do
    other_user = users(:one)
    other_hub = Hub.create!(user: other_user, identifier: "other-hub", last_seen_at: Time.current)

    subscribe hub_id: other_hub.id

    assert subscription.rejected?
  ensure
    other_hub&.destroy
  end

  test "subscribing marks hub alive and updates last_seen_at" do
    @hub.update!(alive: false, last_seen_at: 10.minutes.ago)

    subscribe hub_id: @hub.id

    @hub.reload
    assert @hub.alive?
    assert @hub.last_seen_at > 1.minute.ago
  end

  test "subscribing broadcasts hub health ONLINE" do
    health_stream = "hub:#{@hub.id}:health"

    assert_broadcast_on(health_stream, { type: "health", cli: "online" }) do
      subscribe hub_id: @hub.id
    end
  end

  test "does not stream github events (handled by Github::EventsChannel)" do
    subscribe hub_id: @hub.id

    assert_has_no_stream "github_events:botster/trybotster"
  end

  # === Unsubscribed Tests ===

  test "unsubscribing marks hub as not alive" do
    subscribe hub_id: @hub.id
    @hub.reload
    assert @hub.alive?

    subscription.unsubscribe_from_channel

    @hub.reload
    assert_not @hub.alive?
  end

  test "unsubscribing broadcasts hub health OFFLINE" do
    subscribe hub_id: @hub.id
    health_stream = "hub:#{@hub.id}:health"

    assert_broadcast_on(health_stream, { type: "health", cli: "offline" }) do
      subscription.unsubscribe_from_channel
    end
  end

  # === Replay Tests ===

  test "replays unacked hub commands on subscribe" do
    cmd1 = HubCommand.create_for_hub!(@hub, event_type: "create_agent", payload: { issue_number: 1, prompt: "Test" })
    cmd2 = HubCommand.create_for_hub!(@hub, event_type: "create_agent", payload: { issue_number: 2, prompt: "Test" })
    cmd3 = HubCommand.create_for_hub!(@hub, event_type: "agent_cleanup", payload: { issue_number: 3, reason: "done" })

    cmd1.acknowledge!

    subscribe hub_id: @hub.id, start_from: 0

    assert subscription.confirmed?
    assert_equal 2, transmissions.size
    assert_equal cmd2.sequence, transmissions[0]["sequence"]
    assert_equal cmd3.sequence, transmissions[1]["sequence"]
  end

  test "replays only hub commands after start_from sequence" do
    cmd1 = HubCommand.create_for_hub!(@hub, event_type: "create_agent", payload: { issue_number: 1, prompt: "Test" })
    cmd2 = HubCommand.create_for_hub!(@hub, event_type: "create_agent", payload: { issue_number: 2, prompt: "Test" })
    cmd3 = HubCommand.create_for_hub!(@hub, event_type: "agent_cleanup", payload: { issue_number: 3, reason: "done" })

    subscribe hub_id: @hub.id, start_from: cmd2.sequence

    assert subscription.confirmed?
    assert_equal 1, transmissions.size
    assert_equal cmd3.sequence, transmissions[0]["sequence"]
  end

  test "replay message format includes required fields" do
    cmd = HubCommand.create_for_hub!(@hub, event_type: "create_agent", payload: { issue_number: 42, prompt: "Fix it" })

    subscribe hub_id: @hub.id, start_from: 0

    msg = transmissions.first
    assert_equal "message", msg["type"]
    assert_equal cmd.sequence, msg["sequence"]
    assert_equal cmd.id, msg["id"]
    assert_equal "create_agent", msg["event_type"]
    assert_equal({ "issue_number" => 42, "prompt" => "Fix it" }, msg["payload"])
    assert msg["created_at"].present?
  end

  # === Ack Action Tests ===

  test "ack action acknowledges a hub command" do
    cmd = HubCommand.create_for_hub!(@hub, event_type: "create_agent", payload: { issue_number: 1, prompt: "Test" })

    subscribe hub_id: @hub.id
    perform :ack, sequence: cmd.sequence

    cmd.reload
    assert cmd.acknowledged?
    assert_not_nil cmd.acknowledged_at
  end

  test "ack ignores unknown sequence numbers" do
    subscribe hub_id: @hub.id

    assert_nothing_raised { perform :ack, sequence: 999_999 }
  end

  # === Heartbeat Action Tests ===

  test "heartbeat updates hub alive and last_seen_at" do
    @hub.update!(alive: false, last_seen_at: 10.minutes.ago)

    subscribe hub_id: @hub.id
    perform :heartbeat, agents: []

    @hub.reload
    assert @hub.alive?
    assert @hub.last_seen_at > 1.minute.ago
  end

  # === Signal Action Tests ===

  test "signal relays envelope to browser-specific stream" do
    browser_id = "browser-abc123"
    signal_stream = "hub:#{@hub.id}:signal:#{browser_id}"

    subscribe hub_id: @hub.id

    assert_broadcast_on(signal_stream, { type: "signal", envelope: { "sdp" => "test" } }) do
      perform :signal, browser_identity: browser_id, envelope: { sdp: "test" }
    end
  end

  test "signal does nothing without browser_identity" do
    subscribe hub_id: @hub.id

    assert_nothing_raised { perform :signal, envelope: { sdp: "test" } }
  end
end
