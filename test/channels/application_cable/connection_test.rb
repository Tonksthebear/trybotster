# frozen_string_literal: true

require "test_helper"

class ApplicationCable::ConnectionTest < ActionCable::Connection::TestCase
  setup do
    @user = users(:one)
  end

  test "connects with valid hub token via Authorization header" do
    # Create a hub token for the user
    hub_token = create_hub_token(@user, "Test CLI")

    # Connect with the hub token in Authorization header
    connect headers: { "Authorization" => "Bearer #{hub_token.token}" }

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

  test "connects with btstr_ prefixed hub token" do
    hub_token = create_hub_token(@user, "Prefixed Token")

    # Verify it has the btstr_ prefix
    assert hub_token.token.start_with?("btstr_"), "Token should have btstr_ prefix"

    connect headers: { "Authorization" => "Bearer #{hub_token.token}" }

    assert_equal @user, connection.current_user
  end

  test "hub token from different user does not authenticate as wrong user" do
    other_user = users(:two)
    other_token = create_hub_token(other_user, "Other CLI")

    connect headers: { "Authorization" => "Bearer #{other_token.token}" }

    assert_equal other_user, connection.current_user
    assert_not_equal @user, connection.current_user
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
