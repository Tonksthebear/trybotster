# frozen_string_literal: true

require "test_helper"

class HubChannelTest < ActionCable::Channel::TestCase
  tests HubChannel

  setup do
    @hub_id = 12345  # Numeric hub ID for channel tests
    @browser_identity = "browser-#{SecureRandom.hex(16)}"
  end

  # === Subscription Tests ===

  test "rejects subscription without browser_identity" do
    subscribe hub_id: @hub_id

    assert subscription.rejected?
  end

  test "browser subscribes to hub browser stream" do
    subscribe hub_id: @hub_id, browser_identity: @browser_identity

    assert subscription.confirmed?
    assert_has_stream "hub:#{@hub_id}:browser:#{@browser_identity}"
  end

  test "CLI subscribes to hub CLI stream for specific browser" do
    subscribe hub_id: @hub_id, browser_identity: @browser_identity, cli_subscription: true

    assert subscription.confirmed?
    assert_has_stream "hub:#{@hub_id}:browser:#{@browser_identity}:cli"
  end

  test "rejects subscription without hub_id" do
    subscribe

    assert subscription.rejected?
  end

  test "rejects subscription with blank hub_id" do
    subscribe hub_id: ""

    assert subscription.rejected?
  end

  test "rejects subscription with blank browser_identity" do
    subscribe hub_id: @hub_id, browser_identity: ""

    assert subscription.rejected?
  end

  # === Routing Tests (Per-Browser Bidirectional Streams) ===

  test "browser relay routes to CLI stream for same browser" do
    subscribe hub_id: @hub_id, browser_identity: @browser_identity
    browser_stream = "hub:#{@hub_id}:browser:#{@browser_identity}"
    cli_stream = "hub:#{@hub_id}:browser:#{@browser_identity}:cli"

    # Browser message goes to CLI stream for this browser
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

  test "CLI relay routes to browser stream for same browser" do
    subscribe hub_id: @hub_id, browser_identity: @browser_identity, cli_subscription: true
    browser_stream = "hub:#{@hub_id}:browser:#{@browser_identity}"
    cli_stream = "hub:#{@hub_id}:browser:#{@browser_identity}:cli"

    # CLI message goes to browser stream for this browser
    assert_broadcasts(browser_stream, 1) do
      assert_no_broadcasts(cli_stream) do
        perform :relay, envelope: {
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

  # === Relay Format Tests ===

  test "relay does NOT broadcast when envelope wrapper is missing" do
    subscribe hub_id: @hub_id, browser_identity: @browser_identity
    cli_stream = "hub:#{@hub_id}:browser:#{@browser_identity}:cli"

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
    subscribe hub_id: @hub_id, browser_identity: @browser_identity
    cli_stream = "hub:#{@hub_id}:browser:#{@browser_identity}:cli"

    assert_no_broadcasts(cli_stream) do
      perform :relay, envelope: nil
    end
  end

  # === SenderKey Distribution Tests ===

  test "distribute_sender_key broadcasts to CLI stream for browser" do
    subscribe hub_id: @hub_id, browser_identity: @browser_identity
    cli_stream = "hub:#{@hub_id}:browser:#{@browser_identity}:cli"

    assert_broadcasts(cli_stream, 1) do
      perform :distribute_sender_key, distribution: "base64_sender_key_distribution_message"
    end
  end

  test "distribute_sender_key does NOT broadcast without distribution" do
    subscribe hub_id: @hub_id, browser_identity: @browser_identity
    cli_stream = "hub:#{@hub_id}:browser:#{@browser_identity}:cli"

    assert_no_broadcasts(cli_stream) do
      perform :distribute_sender_key, distribution: nil
    end
  end

  # === Hub-Level Message Tests ===

  test "CLI hub channel handles agent list updates to browser" do
    subscribe hub_id: @hub_id, browser_identity: @browser_identity, cli_subscription: true
    browser_stream = "hub:#{@hub_id}:browser:#{@browser_identity}"

    # CLI sends agent list to specific browser
    assert_broadcasts(browser_stream, 1) do
      perform :relay, envelope: {
        version: 4,
        message_type: 2,
        ciphertext: "encrypted_agent_list",
        sender_identity: "cli_identity_key",
        registration_id: 12345,
        device_id: 1
      }
    end
  end

  # === Browser Connection Notification Tests ===
  #
  # These tests require stub_connection with a real user because
  # notify_cli_of_browser_connection looks up the hub via current_user.

  test "browser subscription creates browser_connected message" do
    user = users(:jason)
    hub = hubs(:active_hub)
    stub_connection current_user: user

    assert_difference "Bot::Message.count", 1 do
      subscribe hub_id: hub.id, browser_identity: @browser_identity
    end

    assert subscription.confirmed?

    msg = Bot::Message.last
    assert_equal "browser_connected", msg.event_type
    assert_equal @browser_identity, msg.payload["browser_identity"]
    assert_equal hub.id, msg.hub_id
  end

  test "browser unsubscription creates browser_disconnected message" do
    user = users(:jason)
    hub = hubs(:active_hub)
    stub_connection current_user: user

    subscribe hub_id: hub.id, browser_identity: @browser_identity
    assert subscription.confirmed?

    assert_difference "Bot::Message.count", 1 do
      unsubscribe
    end

    msg = Bot::Message.last
    assert_equal "browser_disconnected", msg.event_type
    assert_equal @browser_identity, msg.payload["browser_identity"]
    assert_equal hub.id, msg.hub_id
  end

  test "CLI subscription does not create browser_connected message" do
    user = users(:jason)
    hub = hubs(:active_hub)
    stub_connection current_user: user

    # CLI subscriptions (with cli_subscription: true) don't create notifications
    assert_no_difference "Bot::Message.count" do
      subscribe hub_id: hub.id, browser_identity: @browser_identity, cli_subscription: true
    end

    assert subscription.confirmed?
  end

  test "CLI unsubscription does not create browser_disconnected message" do
    user = users(:jason)
    hub = hubs(:active_hub)
    stub_connection current_user: user

    subscribe hub_id: hub.id, browser_identity: @browser_identity, cli_subscription: true
    assert subscription.confirmed?

    # CLI unsubscriptions don't create notifications either
    assert_no_difference "Bot::Message.count" do
      unsubscribe
    end
  end
end
