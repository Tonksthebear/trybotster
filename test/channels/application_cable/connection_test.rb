# frozen_string_literal: true

require "test_helper"

class ApplicationCable::ConnectionTest < ActionCable::Connection::TestCase
  setup do
    @user = users(:one)
  end

  test "connects with valid device token via Authorization header" do
    # Create a device token for the user
    device_token = create_device_token(@user, "Test CLI")

    # Connect with the device token in Authorization header
    connect headers: { "Authorization" => "Bearer #{device_token.token}" }

    assert_equal @user, connection.current_user
  end

  test "rejects connection with invalid token" do
    assert_reject_connection do
      connect headers: { "Authorization" => "Bearer invalid_token" }
    end
  end

  test "rejects connection with empty token" do
    assert_reject_connection do
      connect headers: { "Authorization" => "Bearer " }
    end
  end

  test "rejects connection with no token and no session" do
    assert_reject_connection do
      connect
    end
  end

  test "connects with btstr_ prefixed device token" do
    device_token = create_device_token(@user, "Prefixed Token")

    # Verify it has the btstr_ prefix
    assert device_token.token.start_with?("btstr_"), "Token should have btstr_ prefix"

    connect headers: { "Authorization" => "Bearer #{device_token.token}" }

    assert_equal @user, connection.current_user
  end

  test "device token from different user does not authenticate as wrong user" do
    other_user = users(:two)
    other_token = create_device_token(other_user, "Other CLI")

    connect headers: { "Authorization" => "Bearer #{other_token.token}" }

    assert_equal other_user, connection.current_user
    assert_not_equal @user, connection.current_user
  end

  private

  def create_device_token(user, name)
    device = user.devices.create!(
      name: name,
      device_type: "cli",
      fingerprint: SecureRandom.hex(8).scan(/../).join(":")
    )
    device.create_device_token!(name: name)
  end
end
