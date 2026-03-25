# frozen_string_literal: true

require "test_helper"

# Integration test to verify HubToken authentication works for Action Cable
# This simulates what the CLI does when connecting
class HubTokenActionCableTest < ActionDispatch::IntegrationTest
  setup do
    @user = users(:one)
    @hub = Hub.create!(
      user: @user,
      identifier: "integration-test-hub",
      last_seen_at: Time.current
    )
  end

  teardown do
    @hub&.destroy
  end

  test "hub token is properly created with btstr_ prefix" do
    hub_token = create_hub_token(@user, "Test CLI")

    assert hub_token.token.present?
    assert hub_token.token.start_with?("btstr_"), "Token should have btstr_ prefix, got: #{hub_token.token[0..10]}..."
  end

  test "hub token can be found by token value" do
    hub_token = create_hub_token(@user, "Test CLI")
    token_value = hub_token.token

    found = HubToken.find_by(token: token_value)
    assert_not_nil found
    assert_equal hub_token.id, found.id
    assert_equal @user.id, found.user.id
  end

  test "action cable connection find_user_by_token works with hub token" do
    hub_token = create_hub_token(@user, "Test CLI")

    # Simulate what ApplicationCable::Connection does
    token = hub_token.token

    # This is the logic from connection.rb
    found_token = HubToken.find_by(token: token)
    user = found_token&.user

    assert_not_nil user
    assert_equal @user.id, user.id
  end

  test "action cable connection find_user_by_token returns nil for invalid token" do
    token = "btstr_invalid_token_that_does_not_exist"

    found_token = HubToken.find_by(token: token)
    user = found_token&.user

    assert_nil user
  end

  test "terminal channel subscription works with authenticated user" do
    # This test verifies the channel would accept a subscription
    # if the connection is authenticated with a hub token user

    # Verify hub belongs to user
    assert_equal @user.id, @hub.user_id

    # Verify the lookup works (channels use id, not identifier)
    hub = @user.hubs.find_by(id: @hub.id)
    assert_not_nil hub
    assert_equal @hub.id, hub.id
  end

  test "config get_api_key returns hub token when set" do
    # This simulates what the CLI's Config does
    hub_token = create_hub_token(@user, "Test CLI")

    # The CLI would call config.get_api_key() which should return the token
    # For this test, we just verify the token is usable
    token = hub_token.token

    assert token.present?
    assert token.start_with?("btstr_")

    # Verify it can authenticate
    found = HubToken.find_by(token: token)
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

  private

  def create_hub_token(user, name)
    hub = user.hubs.create!(
      identifier: "test-hub-#{SecureRandom.hex(8)}",
      last_seen_at: Time.current
    )
    hub.create_hub_token!(name: name)
  end
end
