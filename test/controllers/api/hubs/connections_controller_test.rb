# frozen_string_literal: true

require "test_helper"

module Api
  module Hubs
    class ConnectionsControllerTest < ActionDispatch::IntegrationTest
      include Devise::Test::IntegrationHelpers

      setup do
        # User with server_assisted_pairing ENABLED (convenience mode)
        @user_convenience = User.create!(
          email: "connections_convenience_user@example.com",
          username: "connections_convenience_user",
          server_assisted_pairing: true
        )

        # User with server_assisted_pairing DISABLED (secure mode - default)
        @user_secure = User.create!(
          email: "connections_secure_user@example.com",
          username: "connections_secure_user",
          server_assisted_pairing: false
        )

        @other_user = User.create!(
          email: "connections_other_user@example.com",
          username: "connections_other_user"
        )

        @device = Device.create!(
          user: @user_convenience,
          public_key: "test_public_key_base64_for_e2e",
          device_type: "cli",
          name: "Test CLI Device"
        )

        @device_secure = Device.create!(
          user: @user_secure,
          public_key: "test_public_key_base64_for_secure",
          device_type: "cli",
          name: "Test CLI Device Secure"
        )

        @hub_with_device = Hub.create!(
          user: @user_convenience,
          repo: "owner/repo",
          identifier: SecureRandom.uuid,
          last_seen_at: Time.current,
          device: @device
        )

        @hub_without_device = Hub.create!(
          user: @user_convenience,
          repo: "owner/other-repo",
          identifier: SecureRandom.uuid,
          last_seen_at: Time.current,
          device: nil
        )

        @hub_secure = Hub.create!(
          user: @user_secure,
          repo: "secure/repo",
          identifier: SecureRandom.uuid,
          last_seen_at: Time.current,
          device: @device_secure
        )

        @other_user_hub = Hub.create!(
          user: @other_user,
          repo: "other/repo",
          identifier: SecureRandom.uuid,
          last_seen_at: Time.current
        )
      end

      teardown do
        @user_convenience&.destroy
        @user_secure&.destroy
        @other_user&.destroy
      end

      test "show requires authentication" do
        get api_hub_connection_url(hub_identifier: @hub_with_device.identifier),
            as: :json

        assert_response :unauthorized
        json = JSON.parse(response.body)
        assert json["error"].include?("sign in"), "Error should mention signing in"
      end

      # Tests for users with server_assisted_pairing ENABLED (convenience mode)

      test "show returns 422 when hub has no device (convenience mode)" do
        sign_in @user_convenience

        get api_hub_connection_url(hub_identifier: @hub_without_device.identifier),
            as: :json

        assert_response :unprocessable_entity
        json = JSON.parse(response.body)
        assert_equal "Hub has no registered device for E2E encryption", json["error"]
      end

      test "show returns device info when hub has device (convenience mode)" do
        sign_in @user_convenience

        get api_hub_connection_url(hub_identifier: @hub_with_device.identifier),
            as: :json

        assert_response :success
        json = JSON.parse(response.body)

        assert_equal @hub_with_device.id, json["hub_id"]
        assert_equal @hub_with_device.identifier, json["identifier"]
        assert_equal true, json["server_assisted_pairing"]

        device_json = json["device"]
        assert_not_nil device_json, "Response should include device info"
        assert_equal @device.id, device_json["id"]
        assert_equal @device.public_key, device_json["public_key"]
        assert_equal @device.fingerprint, device_json["fingerprint"]
        assert_equal @device.name, device_json["name"]
      end

      test "show returns 404 for non-existent hub" do
        sign_in @user_convenience

        get api_hub_connection_url(hub_identifier: "non-existent-hub"),
            as: :json

        assert_response :not_found
        json = JSON.parse(response.body)
        assert_equal "Hub not found", json["error"]
      end

      test "show returns 404 for other user's hub" do
        sign_in @user_convenience

        get api_hub_connection_url(hub_identifier: @other_user_hub.identifier),
            as: :json

        assert_response :not_found
        json = JSON.parse(response.body)
        assert_equal "Hub not found", json["error"]
      end

      test "show with device contains all required E2E fields (convenience mode)" do
        sign_in @user_convenience

        get api_hub_connection_url(hub_identifier: @hub_with_device.identifier),
            as: :json

        assert_response :success
        json = JSON.parse(response.body)

        # These fields are required for browser to establish E2E encryption
        required_fields = %w[hub_id identifier device server_assisted_pairing]
        required_fields.each do |field|
          assert json.key?(field), "Response must include #{field} for E2E setup"
        end

        device_required_fields = %w[id public_key fingerprint name]
        device_required_fields.each do |field|
          assert json["device"].key?(field), "Device must include #{field} for E2E setup"
        end
      end

      # Tests for users with server_assisted_pairing DISABLED (secure mode - default)

      test "show returns 403 when server_assisted_pairing is disabled (secure mode)" do
        sign_in @user_secure

        get api_hub_connection_url(hub_identifier: @hub_secure.identifier),
            as: :json

        assert_response :forbidden
        json = JSON.parse(response.body)
        assert_equal "Server-assisted pairing is disabled", json["error"]
        assert json["message"].include?("QR code"), "Error should mention QR code"
        assert json["secure_connect_url"].present?, "Should include secure connect URL"
      end

      test "show does not expose public key when server_assisted_pairing is disabled" do
        sign_in @user_secure

        get api_hub_connection_url(hub_identifier: @hub_secure.identifier),
            as: :json

        assert_response :forbidden
        json = JSON.parse(response.body)

        # Ensure the device public key is NOT returned
        assert_nil json["device"], "Should NOT return device info in secure mode"
      end
    end
  end
end
