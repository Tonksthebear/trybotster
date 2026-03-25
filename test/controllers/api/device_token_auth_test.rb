# frozen_string_literal: true

require "test_helper"

module Api
  class HubTokenAuthTest < ActionDispatch::IntegrationTest
    setup do
      @user = User.create!(
        email: "token_auth_test@example.com",
        username: "token_auth_test"
      )
      @hub = @user.hubs.create!(
        identifier: "auth-test-#{SecureRandom.hex(8)}",
        last_seen_at: Time.current
      )
      @hub_token = @hub.create_hub_token!(name: "Test CLI")
    end

    teardown do
      @user&.destroy
    end

    # Test that hub tokens work for authentication

    test "hub token authenticates successfully" do
      get devices_url,
          headers: { "X-API-Key" => @hub_token.token },
          as: :json

      assert_response :success
    end

    test "invalid hub token returns unauthorized" do
      get devices_url,
          headers: { "X-API-Key" => "btstr_invalid_token_here" },
          as: :json

      assert_response :unauthorized
      json = JSON.parse(response.body)
      assert_equal "Invalid API key", json["error"]
    end

    test "invalid token without btstr prefix returns unauthorized" do
      get devices_url,
          headers: { "X-API-Key" => "some_random_token" },
          as: :json

      assert_response :unauthorized
      json = JSON.parse(response.body)
      assert_equal "Invalid API key", json["error"]
    end

    test "missing api key returns unauthorized for json" do
      get devices_url, as: :json

      # DevicesController falls back to Devise session auth
      # For JSON requests without auth, Devise returns 401
      assert_response :unauthorized
    end

    test "hub token updates last_used_at on successful auth" do
      assert_nil @hub_token.last_used_at

      get devices_url,
          headers: { "X-API-Key" => @hub_token.token },
          as: :json

      assert_response :success
      @hub_token.reload
      assert_not_nil @hub_token.last_used_at
    end

    test "hub token records last_ip on successful auth" do
      get devices_url,
          headers: { "X-API-Key" => @hub_token.token },
          as: :json

      assert_response :success
      @hub_token.reload
      assert_not_nil @hub_token.last_ip
    end

    # Test that revoked tokens don't work

    test "deleted hub token returns unauthorized" do
      token_value = @hub_token.token
      @hub_token.destroy

      get devices_url,
          headers: { "X-API-Key" => token_value },
          as: :json

      assert_response :unauthorized
    end
  end
end
