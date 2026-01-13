# frozen_string_literal: true

require "test_helper"

# Tests for TerminalRelayChannel E2E encryption relay.
#
# This channel is a pure relay - it doesn't validate hub ownership.
# It just routes encrypted messages between CLI and browser endpoints.
class TerminalChannelTest < ActionCable::Channel::TestCase
  tests TerminalRelayChannel

  setup do
    @hub_id = 12345
    @browser_identity = "browser-#{SecureRandom.hex(16)}"
  end

  # === Subscription Tests ===

  test "CLI subscribes to CLI stream (no browser_identity)" do
    subscribe hub_id: @hub_id

    assert subscription.confirmed?
    assert_has_stream "terminal_relay:#{@hub_id}:cli"
  end

  test "browser subscribes to dedicated browser stream" do
    subscribe hub_id: @hub_id, browser_identity: @browser_identity

    assert subscription.confirmed?
    assert_has_stream "terminal_relay:#{@hub_id}:browser:#{@browser_identity}"
  end

  test "rejects subscription without hub_id" do
    subscribe

    assert subscription.rejected?
  end

  test "rejects subscription with blank hub_id" do
    subscribe hub_id: ""

    assert subscription.rejected?
  end

  # === Relay Routing Tests ===

  test "CLI relay with recipient_identity routes to browser stream" do
    subscribe hub_id: @hub_id  # Subscribe as CLI

    browser_stream = "terminal_relay:#{@hub_id}:browser:#{@browser_identity}"

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
    subscribe hub_id: @hub_id, browser_identity: @browser_identity

    cli_stream = "terminal_relay:#{@hub_id}:cli"

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
    subscribe hub_id: @hub_id
    cli_stream = "terminal_relay:#{@hub_id}:cli"

    # Missing envelope field
    assert_no_broadcasts(cli_stream) do
      perform :relay, recipient_identity: @browser_identity
    end
  end

  # === SenderKey Distribution Tests ===

  test "distribute_sender_key broadcasts to CLI stream" do
    subscribe hub_id: @hub_id, browser_identity: @browser_identity

    cli_stream = "terminal_relay:#{@hub_id}:cli"

    assert_broadcasts(cli_stream, 1) do
      perform :distribute_sender_key, distribution: "base64_sender_key_distribution_message"
    end
  end

  test "distribute_sender_key does NOT broadcast without distribution" do
    subscribe hub_id: @hub_id

    cli_stream = "terminal_relay:#{@hub_id}:cli"

    assert_no_broadcasts(cli_stream) do
      perform :distribute_sender_key, distribution: nil
    end
  end
end
