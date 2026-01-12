# frozen_string_literal: true

require "test_helper"

class TerminalRelayChannelTest < ActionCable::Channel::TestCase
  tests TerminalRelayChannel

  setup do
    @hub_identifier = "test-hub-#{SecureRandom.hex(4)}"
    @browser_identity = "browser-#{SecureRandom.hex(16)}"
  end

  # === Subscription Tests ===

  test "CLI subscribes to CLI stream (no browser_identity)" do
    subscribe hub_identifier: @hub_identifier

    assert subscription.confirmed?
    assert_has_stream "terminal_relay:#{@hub_identifier}:cli"
  end

  test "browser subscribes to dedicated browser stream" do
    subscribe hub_identifier: @hub_identifier, browser_identity: @browser_identity

    assert subscription.confirmed?
    assert_has_stream "terminal_relay:#{@hub_identifier}:browser:#{@browser_identity}"
  end

  test "rejects subscription without hub_identifier" do
    subscribe

    assert subscription.rejected?
  end

  test "rejects subscription with blank hub_identifier" do
    subscribe hub_identifier: ""

    assert subscription.rejected?
  end

  # === Routing Tests (Server-Side Routing) ===

  test "relay routes to browser stream when recipient_identity present" do
    subscribe hub_identifier: @hub_identifier
    browser_stream = "terminal_relay:#{@hub_identifier}:browser:#{@browser_identity}"
    cli_stream = "terminal_relay:#{@hub_identifier}:cli"

    # Message with recipient_identity goes to that browser's stream
    assert_broadcasts(browser_stream, 1) do
      assert_no_broadcasts(cli_stream) do
        perform :relay, recipient_identity: @browser_identity, envelope: {
          version: 4,
          message_type: 2,
          ciphertext: "base64_encrypted_data",
          sender_identity: "cli_identity_key",
          registration_id: 12345,
          device_id: 1
        }
      end
    end
  end

  test "relay routes to CLI stream when no recipient_identity" do
    subscribe hub_identifier: @hub_identifier, browser_identity: @browser_identity
    browser_stream = "terminal_relay:#{@hub_identifier}:browser:#{@browser_identity}"
    cli_stream = "terminal_relay:#{@hub_identifier}:cli"

    # Message without recipient_identity goes to CLI stream
    assert_broadcasts(cli_stream, 1) do
      assert_no_broadcasts(browser_stream) do
        perform :relay, envelope: {
          version: 4,
          message_type: 1,
          ciphertext: "encrypted_handshake",
          sender_identity: @browser_identity,
          registration_id: 54321,
          device_id: 1
        }
      end
    end
  end

  # === Relay Format Tests ===

  test "relay does NOT broadcast when envelope wrapper is missing" do
    subscribe hub_identifier: @hub_identifier
    cli_stream = "terminal_relay:#{@hub_identifier}:cli"

    # Wrong format: envelope fields at top level
    assert_no_broadcasts(cli_stream) do
      perform :relay,
        version: 4,
        message_type: 2,
        ciphertext: "base64_encrypted_data",
        sender_identity: "signal_identity_key",
        registration_id: 12345,
        device_id: 1
    end
  end

  test "relay does NOT broadcast with nil envelope" do
    subscribe hub_identifier: @hub_identifier
    cli_stream = "terminal_relay:#{@hub_identifier}:cli"

    assert_no_broadcasts(cli_stream) do
      perform :relay, envelope: nil
    end
  end

  # === SenderKey Distribution Tests ===

  test "distribute_sender_key broadcasts to CLI stream" do
    subscribe hub_identifier: @hub_identifier, browser_identity: @browser_identity
    cli_stream = "terminal_relay:#{@hub_identifier}:cli"

    assert_broadcasts(cli_stream, 1) do
      perform :distribute_sender_key, distribution: "base64_sender_key_distribution_message"
    end
  end

  test "distribute_sender_key does NOT broadcast without distribution" do
    subscribe hub_identifier: @hub_identifier
    cli_stream = "terminal_relay:#{@hub_identifier}:cli"

    assert_no_broadcasts(cli_stream) do
      perform :distribute_sender_key, distribution: nil
    end
  end

  # === Signal Protocol Version Tests ===

  test "relay handles PreKeySignalMessage (message_type 1)" do
    subscribe hub_identifier: @hub_identifier, browser_identity: @browser_identity
    cli_stream = "terminal_relay:#{@hub_identifier}:cli"

    # PreKeySignalMessage from browser to CLI (no recipient_identity)
    assert_broadcasts(cli_stream, 1) do
      perform :relay, envelope: {
        version: 4,
        message_type: 1,
        ciphertext: "prekey_signal_message_ciphertext",
        sender_identity: @browser_identity,
        registration_id: 11111,
        device_id: 1
      }
    end
  end

  test "relay handles SignalMessage (message_type 2)" do
    subscribe hub_identifier: @hub_identifier
    browser_stream = "terminal_relay:#{@hub_identifier}:browser:#{@browser_identity}"

    # SignalMessage from CLI to specific browser
    assert_broadcasts(browser_stream, 1) do
      perform :relay, recipient_identity: @browser_identity, envelope: {
        version: 4,
        message_type: 2,
        ciphertext: "signal_message_ciphertext",
        sender_identity: "cli_identity_key",
        registration_id: 11111,
        device_id: 1
      }
    end
  end

  # === Documentation: Expected Message Formats ===
  #
  # CLI -> Browser (with recipient_identity for routing):
  # {
  #   "action": "relay",
  #   "recipient_identity": "browser_identity_key_base64",
  #   "envelope": {
  #     "version": 4,
  #     "message_type": 2,
  #     "ciphertext": "base64_encrypted_data",
  #     "sender_identity": "cli_identity_key",
  #     "registration_id": 12345,
  #     "device_id": 1
  #   }
  # }
  #
  # Browser -> CLI (no recipient_identity):
  # {
  #   "action": "relay",
  #   "envelope": {
  #     "version": 4,
  #     "message_type": 1,
  #     "ciphertext": "base64_encrypted_data",
  #     "sender_identity": "browser_identity_key",
  #     "registration_id": 54321,
  #     "device_id": 1
  #   }
  # }
end
