# frozen_string_literal: true

require "test_helper"

class TerminalChannelTest < ActionCable::Channel::TestCase
  setup do
    @user = users(:one)
    @hub = Hub.create!(
      user: @user,
      identifier: "test-hub-#{SecureRandom.hex(4)}",
      repo: "test/repo",
      last_seen_at: Time.current
    )
  end

  teardown do
    @hub&.destroy
  end

  test "CLI with device token can subscribe to terminal channel" do
    # Create a device token
    device_token = @user.device_tokens.create!(name: "Test CLI")

    # Stub the connection with the user (simulating successful auth)
    stub_connection current_user: @user

    # Subscribe to the channel
    subscribe hub_identifier: @hub.identifier, device_type: "cli"

    assert subscription.confirmed?
    assert_has_stream "terminal_#{@user.id}_#{@hub.identifier}"
  end

  test "browser can subscribe to terminal channel" do
    stub_connection current_user: @user

    subscribe hub_identifier: @hub.identifier, device_type: "browser"

    assert subscription.confirmed?
    assert_has_stream "terminal_#{@user.id}_#{@hub.identifier}"
  end

  test "rejects subscription for hub belonging to different user" do
    other_user = users(:two)
    other_hub = Hub.create!(
      user: other_user,
      identifier: "other-hub-#{SecureRandom.hex(4)}",
      repo: "other/repo",
      last_seen_at: Time.current
    )

    stub_connection current_user: @user

    subscribe hub_identifier: other_hub.identifier, device_type: "cli"

    assert subscription.rejected?

    other_hub.destroy
  end

  test "rejects subscription for non-existent hub" do
    stub_connection current_user: @user

    subscribe hub_identifier: "non-existent-hub", device_type: "cli"

    assert subscription.rejected?
  end

  test "relay broadcasts terminal message with correct from field" do
    stub_connection current_user: @user
    subscribe hub_identifier: @hub.identifier, device_type: "cli"

    stream_name = "terminal_#{@user.id}_#{@hub.identifier}"

    # Check that relay broadcasts with from: cli
    assert_broadcasts(stream_name, 1) do
      perform :relay, blob: "encrypted_data", nonce: "nonce_value"
    end
  end

  test "browser relay broadcasts terminal message" do
    stub_connection current_user: @user
    subscribe hub_identifier: @hub.identifier, device_type: "browser"

    stream_name = "terminal_#{@user.id}_#{@hub.identifier}"

    assert_broadcasts(stream_name, 1) do
      perform :relay, blob: "browser_data", nonce: "browser_nonce"
    end
  end

  test "presence broadcasts device type and public key" do
    stub_connection current_user: @user
    subscribe hub_identifier: @hub.identifier, device_type: "browser"

    stream_name = "terminal_#{@user.id}_#{@hub.identifier}"

    assert_broadcasts(stream_name, 1) do
      perform :presence, event: "join", device_name: "Test Browser", public_key: "test_public_key"
    end
  end

  test "resize broadcasts" do
    stub_connection current_user: @user
    subscribe hub_identifier: @hub.identifier, device_type: "browser"

    stream_name = "terminal_#{@user.id}_#{@hub.identifier}"

    assert_broadcasts(stream_name, 1) do
      perform :resize, cols: 80, rows: 24
    end
  end
end
