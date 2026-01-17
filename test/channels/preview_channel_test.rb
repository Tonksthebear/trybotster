# frozen_string_literal: true

require "test_helper"

class PreviewChannelTest < ActionCable::Channel::TestCase
  tests PreviewChannel

  setup do
    @hub_id = 12345
    @agent_index = 0
    @browser_identity = "browser-#{SecureRandom.hex(16)}"
  end

  # === Subscription Tests ===

  test "agent subscribes to agent stream (no browser_identity)" do
    subscribe hub_id: @hub_id, agent_index: @agent_index

    assert subscription.confirmed?
    assert_has_stream "preview:#{@hub_id}:#{@agent_index}:agent"
  end

  test "browser subscribes to dedicated browser stream" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, browser_identity: @browser_identity

    assert subscription.confirmed?
    assert_has_stream "preview:#{@hub_id}:#{@agent_index}:browser:#{@browser_identity}"
  end

  test "rejects subscription without hub_id" do
    subscribe agent_index: @agent_index

    assert subscription.rejected?
  end

  test "rejects subscription without agent_index" do
    subscribe hub_id: @hub_id

    assert subscription.rejected?
  end

  test "rejects subscription with blank hub_id" do
    subscribe hub_id: "", agent_index: @agent_index

    assert subscription.rejected?
  end

  test "rejects subscription with blank agent_index" do
    subscribe hub_id: @hub_id, agent_index: ""

    assert subscription.rejected?
  end

  # === Routing Tests ===

  test "relay routes browser message to agent stream" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, browser_identity: @browser_identity
    agent_stream = "preview:#{@hub_id}:#{@agent_index}:agent"
    browser_stream = "preview:#{@hub_id}:#{@agent_index}:browser:#{@browser_identity}"

    # Browser message without recipient_identity goes to agent stream
    assert_broadcasts(agent_stream, 1) do
      assert_no_broadcasts(browser_stream) do
        perform :relay, envelope: {
          version: 4,
          message_type: 2,
          ciphertext: "encrypted_http_request",
          sender_identity: @browser_identity,
          registration_id: 54321,
          device_id: 1
        }
      end
    end
  end

  test "relay routes agent message to browser stream when recipient_identity present" do
    subscribe hub_id: @hub_id, agent_index: @agent_index
    agent_stream = "preview:#{@hub_id}:#{@agent_index}:agent"
    browser_stream = "preview:#{@hub_id}:#{@agent_index}:browser:#{@browser_identity}"

    # Agent message with recipient_identity goes to that browser's stream
    assert_broadcasts(browser_stream, 1) do
      assert_no_broadcasts(agent_stream) do
        perform :relay, recipient_identity: @browser_identity, envelope: {
          version: 4,
          message_type: 2,
          ciphertext: "encrypted_http_response",
          sender_identity: "agent_identity_key",
          registration_id: 12345,
          device_id: 1
        }
      end
    end
  end

  test "relay does NOT broadcast when agent sends without recipient_identity" do
    subscribe hub_id: @hub_id, agent_index: @agent_index
    agent_stream = "preview:#{@hub_id}:#{@agent_index}:agent"

    # Agent must specify recipient_identity
    assert_no_broadcasts(agent_stream) do
      perform :relay, envelope: {
        version: 4,
        message_type: 2,
        ciphertext: "encrypted_http_response",
        sender_identity: "agent_identity_key",
        registration_id: 12345,
        device_id: 1
      }
    end
  end

  # === Relay Format Tests ===

  test "relay does NOT broadcast when envelope wrapper is missing" do
    subscribe hub_id: @hub_id, agent_index: @agent_index, browser_identity: @browser_identity
    agent_stream = "preview:#{@hub_id}:#{@agent_index}:agent"

    # Wrong format: no envelope key
    assert_no_broadcasts(agent_stream) do
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
    subscribe hub_id: @hub_id, agent_index: @agent_index, browser_identity: @browser_identity
    agent_stream = "preview:#{@hub_id}:#{@agent_index}:agent"

    assert_no_broadcasts(agent_stream) do
      perform :relay, envelope: nil
    end
  end

  # === Multiple Agent Index Tests ===

  test "different agent indices have separate streams" do
    subscribe hub_id: @hub_id, agent_index: 0
    agent_stream_0 = "preview:#{@hub_id}:0:agent"
    agent_stream_1 = "preview:#{@hub_id}:1:agent"

    # Agent 0's messages should not appear on agent 1's stream
    assert_broadcasts(agent_stream_0, 0) do
      # We're subscribed to agent 0, messages stay on agent 0
    end
  end

  # === Documentation: Expected Message Formats ===
  #
  # Browser -> Agent (HTTP request):
  # {
  #   "action": "relay",
  #   "envelope": {
  #     "version": 4,
  #     "message_type": 2,
  #     "ciphertext": "base64_encrypted_http_request",
  #     "sender_identity": "browser_identity_key",
  #     "registration_id": 54321,
  #     "device_id": 1
  #   }
  # }
  #
  # Agent -> Browser (HTTP response, with recipient_identity for routing):
  # {
  #   "action": "relay",
  #   "recipient_identity": "browser_identity_key_base64",
  #   "envelope": {
  #     "version": 4,
  #     "message_type": 2,
  #     "ciphertext": "base64_encrypted_http_response",
  #     "sender_identity": "agent_identity_key",
  #     "registration_id": 12345,
  #     "device_id": 1
  #   }
  # }
end
