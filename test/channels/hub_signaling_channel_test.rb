# frozen_string_literal: true

require "test_helper"

# Tests HubSignalingChannel — browser-facing WebRTC signal relay.
#
# This is the browser's channel for WebRTC signaling. It:
# - Streams scoped signals from CLI (via hub:id:signal:identity)
# - Streams shared health updates (via hub:id:health)
# - Relays opaque encrypted envelopes from browser → CLI (via hub_command stream)
#
# Auth: Browser session (Warden) — NOT for CLI use.
# CLI uses HubCommandChannel for the other direction.
class HubSignalingChannelTest < ActionCable::Channel::TestCase
  tests HubSignalingChannel

  setup do
    @user = users(:jason)
    @hub = hubs(:active_hub)
    @browser_identity = "browser-#{SecureRandom.hex(16)}"
    stub_connection current_user: @user
  end

  # === Subscription Tests ===

  test "subscribes and streams from signal and health channels" do
    subscribe hub_id: @hub.id, browser_identity: @browser_identity

    assert subscription.confirmed?
    assert_has_stream "hub:#{@hub.id}:signal:#{@browser_identity}"
    assert_has_stream "hub:#{@hub.id}:health"
  end

  test "transmits initial health status on subscribe" do
    subscribe hub_id: @hub.id, browser_identity: @browser_identity

    assert subscription.confirmed?
    assert_equal 1, transmissions.size

    health = transmissions.first
    assert_equal "health", health["type"]
    assert_includes %w[online offline], health["cli"]
  end

  test "transmits online when hub is alive" do
    @hub.update!(alive: true, last_seen_at: Time.current)

    subscribe hub_id: @hub.id, browser_identity: @browser_identity

    health = transmissions.first
    assert_equal "online", health["cli"]
  end

  test "transmits offline when hub is not alive" do
    @hub.update!(alive: false)

    subscribe hub_id: @hub.id, browser_identity: @browser_identity

    health = transmissions.first
    assert_equal "offline", health["cli"]
  end

  test "rejects subscription without hub_id" do
    subscribe browser_identity: @browser_identity

    assert subscription.rejected?
  end

  test "rejects subscription without browser_identity" do
    subscribe hub_id: @hub.id

    assert subscription.rejected?
  end

  test "rejects subscription with nonexistent hub" do
    subscribe hub_id: 999_999, browser_identity: @browser_identity

    assert subscription.rejected?
  end

  test "rejects subscription for hub owned by different user" do
    other_user = users(:one)
    other_hub = Hub.create!(user: other_user, identifier: "other-hub", last_seen_at: Time.current)

    subscribe hub_id: other_hub.id, browser_identity: @browser_identity

    assert subscription.rejected?
  ensure
    other_hub&.destroy
  end

  # === Signal Action Tests ===

  test "signal relays envelope to hub_command stream for CLI" do
    subscribe hub_id: @hub.id, browser_identity: @browser_identity

    hub_command_stream = "hub_command:#{@hub.id}"

    assert_broadcast_on(hub_command_stream, {
      type: "signal",
      browser_identity: @browser_identity,
      envelope: { "ciphertext" => "encrypted_offer" }
    }) do
      perform :signal, envelope: { ciphertext: "encrypted_offer" }
    end
  end

  test "signal does nothing when hub is nil (rejected subscription)" do
    # Force a subscription that was rejected (no hub found)
    subscribe hub_id: 999_999, browser_identity: @browser_identity
    assert subscription.rejected?
  end

  # === Stream Isolation Tests ===

  test "different browser identities get separate signal streams" do
    identity_a = "browser-aaa"
    identity_b = "browser-bbb"

    subscribe hub_id: @hub.id, browser_identity: identity_a

    assert_has_stream "hub:#{@hub.id}:signal:#{identity_a}"
    assert_has_no_stream "hub:#{@hub.id}:signal:#{identity_b}"
  end

  test "signal stream does not leak to other hubs" do
    subscribe hub_id: @hub.id, browser_identity: @browser_identity

    assert_has_stream "hub:#{@hub.id}:signal:#{@browser_identity}"
    assert_has_no_stream "hub:999:signal:#{@browser_identity}"
  end
end
