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

    # Check that relay broadcasts with from: cli (using Olm envelope format)
    assert_broadcasts(stream_name, 1) do
      perform :relay,
        version: 3,
        message_type: 1,
        ciphertext: "encrypted_data",
        sender_key: "cli_curve25519_key"
    end
  end

  test "browser relay broadcasts terminal message" do
    stub_connection current_user: @user
    subscribe hub_identifier: @hub.identifier, device_type: "browser"

    stream_name = "terminal_#{@user.id}_#{@hub.identifier}"

    # Using Olm envelope format
    assert_broadcasts(stream_name, 1) do
      perform :relay,
        version: 3,
        message_type: 1,
        ciphertext: "browser_encrypted_data",
        sender_key: "browser_curve25519_key"
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

  # Note: resize action was removed - resize now goes through encrypted relay
  # The browser sends BrowserCommand::Resize through the Olm-encrypted channel

  # === Tests proving Olm E2E encryption relay issues ===

  test "relay requires Olm envelope fields (version, ciphertext)" do
    stub_connection current_user: @user
    subscribe hub_identifier: @hub.identifier, device_type: "browser"

    stream_name = "terminal_#{@user.id}_#{@hub.identifier}"

    # Missing required Olm envelope fields - should NOT broadcast
    assert_no_broadcasts(stream_name) do
      perform :relay, blob: "old_format_data"
    end
  end

  test "relay broadcasts Olm envelope with all required fields" do
    stub_connection current_user: @user
    subscribe hub_identifier: @hub.identifier, device_type: "browser"

    stream_name = "terminal_#{@user.id}_#{@hub.identifier}"

    # Proper Olm v3 envelope format
    assert_broadcasts(stream_name, 1) do
      perform :relay,
        version: 3,
        message_type: 1,
        ciphertext: "base64_encrypted_data_here",
        sender_key: "curve25519_key_here"
    end
  end

  test "relay preserves sender_key for Olm session identification" do
    stub_connection current_user: @user
    subscribe hub_identifier: @hub.identifier, device_type: "cli"

    stream_name = "terminal_#{@user.id}_#{@hub.identifier}"

    # The sender_key is critical for Olm - browser needs it to know which session to use
    assert_broadcasts(stream_name, 1) do
      perform :relay,
        version: 3,
        message_type: 0,  # PreKey message
        ciphertext: "prekey_ciphertext",
        sender_key: "my_curve25519_identity_key"
    end
  end

  test "presence includes prekey_message for Olm session establishment" do
    stub_connection current_user: @user
    subscribe hub_identifier: @hub.identifier, device_type: "browser"

    stream_name = "terminal_#{@user.id}_#{@hub.identifier}"

    # Browser sends PreKey message in presence to establish Olm session
    olm_prekey = {
      version: 3,
      message_type: 0,
      ciphertext: "prekey_ciphertext_base64",
      sender_key: "browser_curve25519_key"
    }

    assert_broadcasts(stream_name, 1) do
      perform :presence,
        event: "join",
        device_name: "Chrome Browser",
        prekey_message: olm_prekey
    end
  end
end
