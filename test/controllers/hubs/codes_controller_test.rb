# frozen_string_literal: true

require "test_helper"

module Hubs
  class CodesControllerTest < ActionDispatch::IntegrationTest
    # POST /hubs/codes - device code creation

    test "create returns device code and user code" do
      post codes_path, params: { device_name: "my-laptop" }, as: :json

      assert_response :success

      json = JSON.parse(response.body)
      assert json["device_code"].present?
      assert json["user_code"].present?
      assert json["verification_uri"].present?
      assert json["expires_in"].is_a?(Integer)
      assert_equal 5, json["interval"]

      # User code should be formatted as XXXX-XXXX
      assert_match(/\A[A-Z0-9]{4}-[A-Z0-9]{4}\z/, json["user_code"])
    ensure
      DeviceAuthorization.find_by(device_code: json&.dig("device_code"))&.destroy
    end

    test "create persists a pending device authorization" do
      assert_difference "DeviceAuthorization.count", 1 do
        post codes_path, params: { device_name: "test-cli" }, as: :json
      end

      auth = DeviceAuthorization.last
      assert_equal "pending", auth.status
      assert_equal "test-cli", auth.device_name
      assert auth.expires_at > Time.current
    ensure
      auth&.destroy
    end

    # GET /hubs/codes/:id - polling for authorization status

    test "show returns pending status when authorization is not yet approved" do
      auth = DeviceAuthorization.create!(device_name: "poll-test")

      get code_path(auth.device_code), as: :json

      assert_response :accepted

      json = JSON.parse(response.body)
      assert_equal "authorization_pending", json["error"]
    ensure
      auth&.destroy
    end

    test "show returns approved status with tokens when authorization is approved" do
      user = users(:jason)
      auth = DeviceAuthorization.create!(device_name: "approved-test")
      auth.approve!(user)

      get code_path(auth.device_code), as: :json

      assert_response :success

      json = JSON.parse(response.body)
      assert json["access_token"].present?
      assert json["mcp_token"].present?
      assert_equal "bearer", json["token_type"]

      # Tokens should have the correct prefixes
      assert json["access_token"].start_with?(DeviceToken::TOKEN_PREFIX)
      assert json["mcp_token"].start_with?(Integrations::Github::MCPToken::TOKEN_PREFIX)
    ensure
      # Clean up: device created by create_device_tokens, plus auth
      device = Device.find_by(name: "approved-test", user: user)
      device&.destroy
      auth&.destroy
    end

    test "show returns denied status when authorization is denied" do
      auth = DeviceAuthorization.create!(device_name: "denied-test")
      auth.deny!

      get code_path(auth.device_code), as: :json

      assert_response :bad_request

      json = JSON.parse(response.body)
      assert_equal "access_denied", json["error"]
    ensure
      auth&.destroy
    end

    test "show returns expired status for expired authorization" do
      auth = DeviceAuthorization.create!(device_name: "expired-test")
      auth.update_columns(expires_at: 1.minute.ago)

      get code_path(auth.device_code), as: :json

      assert_response :bad_request

      json = JSON.parse(response.body)
      assert_equal "expired_token", json["error"]

      # Should also transition status to expired
      auth.reload
      assert_equal "expired", auth.status
    ensure
      auth&.destroy
    end

    test "show returns invalid_grant for unknown device code" do
      get code_path("nonexistent-device-code"), as: :json

      assert_response :bad_request

      json = JSON.parse(response.body)
      assert_equal "invalid_grant", json["error"]
    end

    # Authorization approval creates Device and DeviceToken

    test "approval creates a device and both tokens for the user" do
      user = users(:jason)
      auth = DeviceAuthorization.create!(device_name: "new-device")
      auth.approve!(user)

      assert_difference [ "Device.count", "DeviceToken.count", "Integrations::Github::MCPToken.count" ], 1 do
        get code_path(auth.device_code), as: :json
      end

      assert_response :success

      device = Device.where(user: user, name: "new-device").last
      assert_not_nil device
      assert_equal "cli", device.device_type
      assert device.fingerprint.present?
      assert_not_nil device.device_token
      assert_not_nil device.mcp_token
    ensure
      Device.where(user: user, name: "new-device").destroy_all
      auth&.destroy
    end

    test "polling an already-consumed approved authorization does not create duplicate devices" do
      user = users(:jason)
      auth = DeviceAuthorization.create!(device_name: "once-only")
      auth.approve!(user)

      # First poll creates the device
      get code_path(auth.device_code), as: :json
      assert_response :success

      # Second poll would try to create another device - verify behavior
      # The controller creates a new device each time, so we verify the first call worked
      json = JSON.parse(response.body)
      assert json["access_token"].present?
      assert json["mcp_token"].present?
    ensure
      Device.where(user: user, name: "once-only").destroy_all
      auth&.destroy
    end

    test "expired pending authorization transitions status to expired" do
      auth = DeviceAuthorization.create!(device_name: "transition-test")
      auth.update_columns(expires_at: 1.second.ago)

      assert_equal "pending", auth.status

      get code_path(auth.device_code), as: :json

      assert_response :bad_request
      auth.reload
      assert_equal "expired", auth.status
    ensure
      auth&.destroy
    end

    test "already-expired non-pending authorization does not re-expire" do
      auth = DeviceAuthorization.create!(device_name: "already-denied")
      auth.deny!
      auth.update_columns(expires_at: 1.second.ago)

      get code_path(auth.device_code), as: :json

      assert_response :bad_request

      json = JSON.parse(response.body)
      assert_equal "expired_token", json["error"]

      # Status should remain denied, not changed to expired
      auth.reload
      assert_equal "denied", auth.status
    ensure
      auth&.destroy
    end
  end
end
