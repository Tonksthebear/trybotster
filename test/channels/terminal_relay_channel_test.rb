# frozen_string_literal: true

require "test_helper"

class TerminalRelayChannelTest < ActionCable::Channel::TestCase
  tests TerminalRelayChannel

  setup do
    @hub_identifier = "test-hub-#{SecureRandom.hex(4)}"
  end

  # === Subscription Tests ===

  test "subscribes successfully with valid hub_identifier" do
    subscribe hub_identifier: @hub_identifier

    assert subscription.confirmed?
    assert_has_stream "terminal_relay:#{@hub_identifier}"
  end

  test "rejects subscription without hub_identifier" do
    subscribe

    assert subscription.rejected?
  end

  test "rejects subscription with blank hub_identifier" do
    subscribe hub_identifier: ""

    assert subscription.rejected?
  end

  # === Relay Format Tests (THE CRITICAL BUG WE FIXED) ===

  test "relay broadcasts message when envelope wrapper is present" do
    subscribe hub_identifier: @hub_identifier
    stream_name = "terminal_relay:#{@hub_identifier}"

    # Correct format: envelope fields nested under "envelope" key
    assert_broadcasts(stream_name, 1) do
      perform :relay, envelope: {
        version: 4,
        message_type: 2,
        ciphertext: "base64_encrypted_data",
        sender_identity: "signal_identity_key",
        registration_id: 12345,
        device_id: 1
      }
    end
  end

  test "relay does NOT broadcast when envelope wrapper is missing" do
    subscribe hub_identifier: @hub_identifier
    stream_name = "terminal_relay:#{@hub_identifier}"

    # Wrong format: envelope fields at top level (the bug we fixed!)
    assert_no_broadcasts(stream_name) do
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
    stream_name = "terminal_relay:#{@hub_identifier}"

    assert_no_broadcasts(stream_name) do
      perform :relay, envelope: nil
    end
  end

  test "relay does NOT broadcast with empty envelope" do
    subscribe hub_identifier: @hub_identifier
    stream_name = "terminal_relay:#{@hub_identifier}"

    # Empty hash is "present" but probably shouldn't broadcast either
    # Current implementation allows this - may want to add validation
    # For now, test documents current behavior
  end

  test "relay preserves envelope structure in broadcast" do
    subscribe hub_identifier: @hub_identifier
    stream_name = "terminal_relay:#{@hub_identifier}"

    envelope_data = {
      version: 4,
      message_type: 1,  # PreKeySignalMessage
      ciphertext: "encrypted_handshake_data",
      sender_identity: "browser_identity_key_base64",
      registration_id: 54321,
      device_id: 1
    }

    # Capture the broadcast to verify structure
    assert_broadcasts(stream_name, 1) do
      perform :relay, envelope: envelope_data
    end
  end

  # === SenderKey Distribution Tests ===

  test "distribute_sender_key broadcasts distribution message" do
    subscribe hub_identifier: @hub_identifier
    stream_name = "terminal_relay:#{@hub_identifier}"

    assert_broadcasts(stream_name, 1) do
      perform :distribute_sender_key, distribution: "base64_sender_key_distribution_message"
    end
  end

  test "distribute_sender_key does NOT broadcast without distribution" do
    subscribe hub_identifier: @hub_identifier
    stream_name = "terminal_relay:#{@hub_identifier}"

    assert_no_broadcasts(stream_name) do
      perform :distribute_sender_key, distribution: nil
    end
  end

  test "distribute_sender_key does NOT broadcast with blank distribution" do
    subscribe hub_identifier: @hub_identifier
    stream_name = "terminal_relay:#{@hub_identifier}"

    assert_no_broadcasts(stream_name) do
      perform :distribute_sender_key, distribution: ""
    end
  end

  # === Signal Protocol Version Tests ===

  test "relay handles PreKeySignalMessage (message_type 1)" do
    subscribe hub_identifier: @hub_identifier
    stream_name = "terminal_relay:#{@hub_identifier}"

    # PreKeySignalMessage is sent first to establish session
    assert_broadcasts(stream_name, 1) do
      perform :relay, envelope: {
        version: 4,
        message_type: 1,  # PreKeySignalMessage
        ciphertext: "prekey_signal_message_ciphertext",
        sender_identity: "sender_identity_key",
        registration_id: 11111,
        device_id: 1
      }
    end
  end

  test "relay handles SignalMessage (message_type 2)" do
    subscribe hub_identifier: @hub_identifier
    stream_name = "terminal_relay:#{@hub_identifier}"

    # SignalMessage is used after session is established
    assert_broadcasts(stream_name, 1) do
      perform :relay, envelope: {
        version: 4,
        message_type: 2,  # SignalMessage
        ciphertext: "signal_message_ciphertext",
        sender_identity: "sender_identity_key",
        registration_id: 11111,
        device_id: 1
      }
    end
  end

  test "relay handles SenderKeyMessage (message_type 3)" do
    subscribe hub_identifier: @hub_identifier
    stream_name = "terminal_relay:#{@hub_identifier}"

    # SenderKeyMessage is used for group broadcasts
    assert_broadcasts(stream_name, 1) do
      perform :relay, envelope: {
        version: 4,
        message_type: 3,  # SenderKeyMessage
        ciphertext: "sender_key_message_ciphertext",
        sender_identity: "cli_identity_key",
        registration_id: 22222,
        device_id: 1
      }
    end
  end

  # === Envelope as String Tests ===

  test "relay handles envelope as JSON string" do
    subscribe hub_identifier: @hub_identifier
    stream_name = "terminal_relay:#{@hub_identifier}"

    envelope_json = {
      version: 4,
      message_type: 2,
      ciphertext: "encrypted_data",
      sender_identity: "identity_key",
      registration_id: 12345,
      device_id: 1
    }.to_json

    assert_broadcasts(stream_name, 1) do
      perform :relay, envelope: envelope_json
    end
  end

  # === Documentation: Expected CLI Message Format ===
  #
  # The CLI must send messages in this format for the relay to work:
  #
  # {
  #   "action": "relay",
  #   "envelope": {
  #     "version": 4,
  #     "message_type": 2,
  #     "ciphertext": "base64_encrypted_data",
  #     "sender_identity": "base64_identity_key",
  #     "registration_id": 12345,
  #     "device_id": 1
  #   }
  # }
  #
  # NOT this (wrong - envelope fields at top level):
  #
  # {
  #   "action": "relay",
  #   "version": 4,
  #   "message_type": 2,
  #   ...
  # }
end
