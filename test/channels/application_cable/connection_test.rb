# frozen_string_literal: true

require "test_helper"

class ApplicationCable::ConnectionTest < ActionCable::Connection::TestCase
  setup do
    @user = users(:one)
  end

  test "connects with valid device token" do
    # Create a device token for the user
    device_token = @user.device_tokens.create!(name: "Test CLI")

    # Connect with the device token
    connect params: { api_key: device_token.token }

    assert_equal @user, connection.current_user
  end

  test "rejects connection with invalid token" do
    assert_reject_connection do
      connect params: { api_key: "invalid_token" }
    end
  end

  test "rejects connection with empty token" do
    assert_reject_connection do
      connect params: { api_key: "" }
    end
  end

  test "rejects connection with no token and no session" do
    assert_reject_connection do
      connect
    end
  end

  test "connects with btstr_ prefixed device token" do
    device_token = @user.device_tokens.create!(name: "Prefixed Token")

    # Verify it has the btstr_ prefix
    assert device_token.token.start_with?("btstr_"), "Token should have btstr_ prefix"

    connect params: { api_key: device_token.token }

    assert_equal @user, connection.current_user
  end

  test "device token from different user does not authenticate as wrong user" do
    other_user = users(:two)
    other_token = other_user.device_tokens.create!(name: "Other CLI")

    connect params: { api_key: other_token.token }

    assert_equal other_user, connection.current_user
    assert_not_equal @user, connection.current_user
  end
end
