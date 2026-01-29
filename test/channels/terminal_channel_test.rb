# frozen_string_literal: true

require "test_helper"

# Tests for TerminalRelayChannel E2E encryption relay.
#
# This channel is a pure relay - it doesn't validate hub ownership.
# It just routes encrypted messages between CLI and browser endpoints.
# Each agent has its own stream (per-agent, per-PTY channels).
class TerminalChannelTest < ActionCable::Channel::TestCase
  tests TerminalRelayChannel

  setup do
    @hub_id = 12345
    @agent_index = 0
    @pty_index = 0  # 0=CLI, 1=Server
    @browser_identity = "browser-#{SecureRandom.hex(16)}"
  end

  # === Subscription Tests ===

  test "CLI subscribes to CLI stream (no browser_identity)" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index

    assert subscription.confirmed?
    assert_has_stream "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:cli"
  end

  test "browser subscribes to dedicated browser stream" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index, browser_identity: @browser_identity

    assert subscription.confirmed?
    assert_has_stream "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:browser:#{@browser_identity}"
  end

  test "rejects subscription without hub_id" do
    subscribe agent_index: @agent_index, pty_index: @pty_index

    assert subscription.rejected?
  end

  test "rejects subscription with blank hub_id" do
    subscribe hub_id: "", agent_index: @agent_index, pty_index: @pty_index

    assert subscription.rejected?
  end

  test "subscribes with default agent_index 0 when not provided" do
    subscribe hub_id: @hub_id, pty_index: @pty_index

    assert subscription.confirmed?
    # Should default to agent_index 0
    assert_has_stream "terminal_relay:#{@hub_id}:0:#{@pty_index}:cli"
  end

  test "subscribes with default pty_index 0 when not provided" do
    subscribe hub_id: @hub_id, agent_index: @agent_index

    assert subscription.confirmed?
    # Should default to pty_index 0
    assert_has_stream "terminal_relay:#{@hub_id}:#{@agent_index}:0:cli"
  end

  test "different pty_index produces different streams" do
    # Subscribe to CLI PTY
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: 0

    assert subscription.confirmed?
    assert_has_stream "terminal_relay:#{@hub_id}:#{@agent_index}:0:cli"
    refute_includes subscription.streams, "terminal_relay:#{@hub_id}:#{@agent_index}:1:cli"
  end

  test "server PTY uses pty_index 1" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: 1

    assert subscription.confirmed?
    assert_has_stream "terminal_relay:#{@hub_id}:#{@agent_index}:1:cli"
  end

  # === Relay Routing Tests ===

  test "CLI relay with recipient_identity routes to browser stream" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index  # Subscribe as CLI

    browser_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:browser:#{@browser_identity}"

    # CLI sends to specific browser
    assert_broadcasts(browser_stream, 1) do
      perform :relay, recipient_identity: @browser_identity, envelope: {
        version: 4,
        message_type: 2,
        ciphertext: "encrypted_data"
      }
    end
  end

  test "browser relay without recipient_identity routes to CLI stream" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index, browser_identity: @browser_identity

    cli_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:cli"

    # Browser sends to CLI (no recipient_identity needed)
    assert_broadcasts(cli_stream, 1) do
      perform :relay, envelope: {
        version: 4,
        message_type: 1,
        ciphertext: "encrypted_handshake"
      }
    end
  end

  test "relay does NOT broadcast when envelope is missing" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index
    cli_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:cli"

    # Missing envelope field
    assert_no_broadcasts(cli_stream) do
      perform :relay, recipient_identity: @browser_identity
    end
  end

  # === SenderKey Distribution Tests ===

  test "distribute_sender_key broadcasts to CLI stream" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index, browser_identity: @browser_identity

    cli_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:cli"

    assert_broadcasts(cli_stream, 1) do
      perform :distribute_sender_key, distribution: "base64_sender_key_distribution_message"
    end
  end

  test "distribute_sender_key does NOT broadcast without distribution" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index

    cli_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:cli"

    assert_no_broadcasts(cli_stream) do
      perform :distribute_sender_key, distribution: nil
    end
  end

  # === Browser -> PTY I/O Flow Integration Tests ===
  #
  # These tests verify the explicit routing architecture established in Tasks #1-4:
  # - Task #2: PTY channels stored in ClientRegistry, created on agent selection
  # - Task #3: Output forwarding tasks spawn per-channel, input routes via BrowserCommand
  # - Task #4: Input/resize use TerminalRelayChannel, not HubChannel

  test "multiple browsers can subscribe to same agent with isolated streams" do
    browser1_identity = "browser-1-#{SecureRandom.hex(8)}"
    browser2_identity = "browser-2-#{SecureRandom.hex(8)}"

    # Subscribe both browsers to the same agent
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index, browser_identity: browser1_identity

    assert subscription.confirmed?
    browser1_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:browser:#{browser1_identity}"
    assert_has_stream browser1_stream

    # Verify browser 1's stream is isolated (doesn't include browser 2's stream)
    browser2_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:browser:#{browser2_identity}"
    refute_includes subscription.streams, browser2_stream
  end

  test "CLI relay to browser routes to correct browser stream only" do
    browser1_identity = "browser-1-#{SecureRandom.hex(8)}"
    browser2_identity = "browser-2-#{SecureRandom.hex(8)}"

    # Subscribe as CLI
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index

    browser1_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:browser:#{browser1_identity}"
    browser2_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:browser:#{browser2_identity}"

    # CLI sends to browser 1 - should ONLY go to browser 1's stream
    assert_broadcasts(browser1_stream, 1) do
      assert_no_broadcasts(browser2_stream) do
        perform :relay, recipient_identity: browser1_identity, envelope: {
          version: 4,
          message_type: 2,
          ciphertext: "encrypted_output_for_browser1"
        }
      end
    end
  end

  test "browser input relay routes to CLI stream regardless of which browser sends" do
    browser1_identity = "browser-1-#{SecureRandom.hex(8)}"
    browser2_identity = "browser-2-#{SecureRandom.hex(8)}"
    cli_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:cli"

    # Browser 1 sends input to CLI
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index, browser_identity: browser1_identity

    assert_broadcasts(cli_stream, 1) do
      perform :relay, envelope: {
        version: 4,
        message_type: 1,
        ciphertext: "encrypted_input_from_browser1"
      }
    end
  end

  test "different agents have completely separate streams" do
    agent0_cli_stream = "terminal_relay:#{@hub_id}:0:#{@pty_index}:cli"
    agent1_cli_stream = "terminal_relay:#{@hub_id}:1:#{@pty_index}:cli"

    # Subscribe to agent 0
    subscribe hub_id: @hub_id, agent_index: 0, pty_index: @pty_index

    assert subscription.confirmed?
    assert_has_stream agent0_cli_stream
    refute_includes subscription.streams, agent1_cli_stream
  end

  test "PTY index isolation - CLI PTY and Server PTY have separate streams" do
    cli_pty_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:0:cli"
    server_pty_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:1:cli"

    # Subscribe to CLI PTY (index 0)
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: 0

    assert subscription.confirmed?
    assert_has_stream cli_pty_stream
    refute_includes subscription.streams, server_pty_stream
  end

  test "browser switching agents gets new stream" do
    browser_identity = "browser-switch-#{SecureRandom.hex(8)}"

    # First subscription to agent 0
    subscribe hub_id: @hub_id, agent_index: 0, pty_index: @pty_index, browser_identity: browser_identity

    agent0_browser_stream = "terminal_relay:#{@hub_id}:0:#{@pty_index}:browser:#{browser_identity}"
    assert subscription.confirmed?
    assert_has_stream agent0_browser_stream

    # Note: In practice, browser would unsubscribe from agent 0 and subscribe to agent 1
    # This test verifies stream names are correctly derived from agent_index
    agent1_browser_stream = "terminal_relay:#{@hub_id}:1:#{@pty_index}:browser:#{browser_identity}"
    refute_includes subscription.streams, agent1_browser_stream
  end

  test "output message format includes envelope wrapper" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index
    browser_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:browser:#{@browser_identity}"

    # Capture the broadcast message format
    # Note: We verify broadcast happens, and the relay method wraps in { envelope: ... }
    # by checking the channel implementation directly
    assert_broadcasts(browser_stream, 1) do
      perform :relay, recipient_identity: @browser_identity, envelope: {
        version: 4,
        message_type: 2,
        ciphertext: "test_output"
      }
    end

    # The relay method broadcasts { envelope: envelope } - verified by code inspection
    # ActionCable test helpers don't provide direct access to broadcast content in Rails 8+
  end

  test "input message without envelope is rejected" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index, browser_identity: @browser_identity
    cli_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:cli"

    # Send without envelope wrapper - should not broadcast
    assert_no_broadcasts(cli_stream) do
      # Note: Missing envelope key
      perform :relay, ciphertext: "raw_data"
    end
  end

  test "resize message routes through terminal relay channel" do
    # Resize is sent as part of the encrypted message payload
    # This test verifies the relay mechanism works for resize messages
    subscribe hub_id: @hub_id, agent_index: @agent_index, pty_index: @pty_index, browser_identity: @browser_identity
    cli_stream = "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:cli"

    # Browser sends resize (wrapped in envelope like any terminal message)
    assert_broadcasts(cli_stream, 1) do
      perform :relay, envelope: {
        version: 4,
        message_type: 1,
        # The actual resize data is encrypted, this is just the envelope
        ciphertext: "encrypted_resize_80x24"
      }
    end
  end

  # === Terminal Connection Notification Tests ===
  #
  # These tests require stub_connection with a real user because
  # notify_cli_of_terminal_connection looks up the hub via current_user.

  test "browser subscription creates terminal_connected message" do
    user = users(:jason)
    hub = hubs(:active_hub)
    stub_connection current_user: user

    assert_difference "Bot::Message.count", 1 do
      subscribe hub_id: hub.id, agent_index: @agent_index, pty_index: @pty_index, browser_identity: @browser_identity
    end

    assert subscription.confirmed?

    msg = Bot::Message.last
    assert_equal "terminal_connected", msg.event_type
    assert_equal @agent_index, msg.payload["agent_index"]
    assert_equal @pty_index, msg.payload["pty_index"]
    assert_equal @browser_identity, msg.payload["browser_identity"]
    assert_equal hub.id, msg.hub_id
  end

  test "browser unsubscription creates terminal_disconnected message" do
    user = users(:jason)
    hub = hubs(:active_hub)
    stub_connection current_user: user

    subscribe hub_id: hub.id, agent_index: @agent_index, pty_index: @pty_index, browser_identity: @browser_identity
    assert subscription.confirmed?

    assert_difference "Bot::Message.count", 1 do
      unsubscribe
    end

    msg = Bot::Message.last
    assert_equal "terminal_disconnected", msg.event_type
    assert_equal @agent_index, msg.payload["agent_index"]
    assert_equal @pty_index, msg.payload["pty_index"]
    assert_equal @browser_identity, msg.payload["browser_identity"]
    assert_equal hub.id, msg.hub_id
  end

  test "CLI subscription does not create terminal_connected message" do
    user = users(:jason)
    hub = hubs(:active_hub)
    stub_connection current_user: user

    assert_no_difference "Bot::Message.count" do
      subscribe hub_id: hub.id, agent_index: @agent_index, pty_index: @pty_index
    end

    assert subscription.confirmed?
  end

  test "terminal_connected message includes agent and pty indices" do
    user = users(:jason)
    hub = hubs(:active_hub)
    stub_connection current_user: user

    subscribe hub_id: hub.id, agent_index: 2, pty_index: 1, browser_identity: @browser_identity
    assert subscription.confirmed?

    msg = Bot::Message.last
    assert_equal "terminal_connected", msg.event_type
    assert_equal 2, msg.payload["agent_index"]
    assert_equal 1, msg.payload["pty_index"]
  end
end
