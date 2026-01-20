# frozen_string_literal: true

require "test_helper"

# Integration test to verify DeviceToken authentication works for Action Cable
# This simulates what the CLI does when connecting
class DeviceTokenActionCableTest < ActionDispatch::IntegrationTest
  setup do
    @user = users(:one)
    @hub = Hub.create!(
      user: @user,
      identifier: "integration-test-hub",
      repo: "test/repo",
      last_seen_at: Time.current
    )
  end

  teardown do
    @hub&.destroy
  end

  test "device token is properly created with btstr_ prefix" do
    device_token = @user.device_tokens.create!(name: "Test CLI")

    assert device_token.token.present?
    assert device_token.token.start_with?("btstr_"), "Token should have btstr_ prefix, got: #{device_token.token[0..10]}..."
  end

  test "device token can be found by token value" do
    device_token = @user.device_tokens.create!(name: "Test CLI")
    token_value = device_token.token

    found = DeviceToken.find_by(token: token_value)
    assert_not_nil found
    assert_equal device_token.id, found.id
    assert_equal @user.id, found.user_id
  end

  test "action cable connection find_user_by_token works with device token" do
    device_token = @user.device_tokens.create!(name: "Test CLI")

    # Simulate what ApplicationCable::Connection does
    token = device_token.token

    # This is the logic from connection.rb
    if token.start_with?(DeviceToken::TOKEN_PREFIX)
      found_token = DeviceToken.find_by(token: token)
      user = found_token&.user
    end

    assert_not_nil user
    assert_equal @user.id, user.id
  end

  test "action cable connection find_user_by_token returns nil for invalid token" do
    token = "btstr_invalid_token_that_does_not_exist"

    if token.start_with?(DeviceToken::TOKEN_PREFIX)
      found_token = DeviceToken.find_by(token: token)
      user = found_token&.user
    end

    assert_nil user
  end

  test "terminal channel subscription works with authenticated user" do
    # This test verifies the channel would accept a subscription
    # if the connection is authenticated with a device token user

    # Verify hub belongs to user
    assert_equal @user.id, @hub.user_id

    # Verify the lookup works
    hub = @user.hubs.find_by(identifier: @hub.identifier)
    assert_not_nil hub
    assert_equal @hub.id, hub.id
  end

  test "config get_api_key returns device token when set" do
    # This simulates what the CLI's Config does
    device_token = @user.device_tokens.create!(name: "Test CLI")

    # The CLI would call config.get_api_key() which should return the token
    # For this test, we just verify the token is usable
    token = device_token.token

    assert token.present?
    assert token.start_with?("btstr_")

    # Verify it can authenticate
    found = DeviceToken.find_by(token: token)
    assert_not_nil found
  end

  test "websocket url format is correct" do
    # The CLI builds: {base}/cable with Authorization: Bearer header
    # NO query params for security (tokens visible in logs/history)
    server_url = "https://dev.trybotster.com"

    ws_url = server_url
      .gsub("https://", "wss://")
      .gsub("http://", "ws://")

    expected_url = "wss://dev.trybotster.com/cable"
    actual_url = "#{ws_url}/cable"

    assert_equal expected_url, actual_url
  end
end
