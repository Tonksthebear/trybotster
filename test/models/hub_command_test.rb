# frozen_string_literal: true

require "test_helper"

class HubCommandTest < ActiveSupport::TestCase
  include ActionCable::TestHelper

  test "create_for_hub! creates command with hub association and sequence" do
    hub = hubs(:active_hub)

    cmd = HubCommand.create_for_hub!(hub,
      event_type: "browser_wants_preview",
      payload: { browser_identity: "test-browser", agent_index: 0, pty_index: 1 })

    assert cmd.persisted?
    assert_equal hub, cmd.hub
    assert_equal "browser_wants_preview", cmd.event_type
    assert_equal "pending", cmd.status
    assert_not_nil cmd.sequence
    assert cmd.sequence > 0
  end

  test "create_for_hub! assigns sequential sequence numbers" do
    hub = hubs(:active_hub)

    cmd1 = HubCommand.create_for_hub!(hub,
      event_type: "browser_wants_preview",
      payload: { browser_identity: "b1" })
    cmd2 = HubCommand.create_for_hub!(hub,
      event_type: "browser_wants_preview",
      payload: { browser_identity: "b2" })

    assert_equal cmd1.sequence + 1, cmd2.sequence
  end

  test "create_for_hub! broadcasts to hub command channel" do
    hub = hubs(:active_hub)

    assert_broadcasts("hub_command:#{hub.id}", 1) do
      HubCommand.create_for_hub!(hub,
        event_type: "browser_wants_preview",
        payload: { browser_identity: "test-broadcast" })
    end
  end

  test "acknowledge! updates status" do
    hub = hubs(:active_hub)
    cmd = HubCommand.create_for_hub!(hub,
      event_type: "browser_wants_preview",
      payload: { browser_identity: "b1" })

    cmd.acknowledge!

    assert_equal "acknowledged", cmd.status
    assert_not_nil cmd.acknowledged_at
  end

  test "unacked_from scope returns unacked commands after sequence" do
    hub = hubs(:active_hub)

    cmd1 = HubCommand.create_for_hub!(hub,
      event_type: "browser_wants_preview",
      payload: { browser_identity: "b1" })
    cmd2 = HubCommand.create_for_hub!(hub,
      event_type: "browser_wants_preview",
      payload: { browser_identity: "b2" })
    cmd3 = HubCommand.create_for_hub!(hub,
      event_type: "browser_wants_preview",
      payload: { browser_identity: "b3" })

    cmd1.acknowledge!

    results = hub.hub_commands.unacked_from(0)
    refute_includes results, cmd1
    assert_includes results, cmd2
    assert_includes results, cmd3
  end

  test "validates event_type inclusion" do
    hub = hubs(:active_hub)
    cmd = HubCommand.new(
      hub: hub,
      event_type: "invalid_type",
      payload: { foo: "bar" },
      sequence: 1
    )
    refute cmd.valid?
  end
end
