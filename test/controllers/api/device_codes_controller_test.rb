# frozen_string_literal: true

require "test_helper"

module Api
  class DeviceCodesControllerTest < ActionDispatch::IntegrationTest
    setup do
      @user = User.create!(
        email: "device_test_user@example.com",
        username: "device_test_user"
      )
      @user.generate_api_key
      @user.save!
    end

    teardown do
      DeviceAuthorization.where(user: @user).destroy_all
      DeviceAuthorization.where(user: nil).destroy_all
      DeviceToken.where(user: @user).destroy_all
      @user&.destroy
    end

    # POST /api/device_codes - Create device authorization

    test "create returns device code and user code" do
      post api_device_codes_url, as: :json

      assert_response :success
      json = JSON.parse(response.body)

      assert json["device_code"].present?
      assert json["user_code"].present?
      assert json["verification_uri"].present?
      assert json["expires_in"].present?
      assert json["interval"].present?

      # User code should be formatted like XXXX-XXXX
      assert_match(/\A[A-Z0-9]{4}-[A-Z0-9]{4}\z/, json["user_code"])

      # Verify a DeviceAuthorization was created
      auth = DeviceAuthorization.find_by(device_code: json["device_code"])
      assert auth.present?
      assert_equal "pending", auth.status
    end

    test "create accepts optional device_name" do
      post api_device_codes_url,
           params: { device_name: "My MacBook Pro" },
           as: :json

      assert_response :success

      auth = DeviceAuthorization.last
      assert_equal "My MacBook Pro", auth.device_name
    end

    # GET /api/device_codes/:device_code - Poll for token

    test "show returns authorization_pending for pending request" do
      auth = DeviceAuthorization.create!

      get api_device_code_url(device_code: auth.device_code), as: :json

      assert_response :accepted  # 202
      json = JSON.parse(response.body)
      assert_equal "authorization_pending", json["error"]
    end

    test "show returns access_token when approved" do
      auth = DeviceAuthorization.create!
      auth.approve!(@user)

      get api_device_code_url(device_code: auth.device_code), as: :json

      assert_response :success
      json = JSON.parse(response.body)
      assert json["access_token"].present?
      assert_equal "bearer", json["token_type"]

      # Verify token starts with btstr_ prefix
      assert json["access_token"].start_with?("btstr_")

      # Verify a DeviceToken was created
      token = DeviceToken.find_by(token: json["access_token"])
      assert token.present?
      assert_equal @user, token.user
    end

    test "show returns access_denied when denied" do
      auth = DeviceAuthorization.create!
      auth.deny!

      get api_device_code_url(device_code: auth.device_code), as: :json

      assert_response :bad_request
      json = JSON.parse(response.body)
      assert_equal "access_denied", json["error"]
    end

    test "show returns expired_token when expired" do
      auth = DeviceAuthorization.create!(expires_at: 1.hour.ago)

      get api_device_code_url(device_code: auth.device_code), as: :json

      assert_response :bad_request
      json = JSON.parse(response.body)
      assert_equal "expired_token", json["error"]

      # Verify status was updated to expired
      auth.reload
      assert_equal "expired", auth.status
    end

    test "show returns invalid_grant for unknown device_code" do
      get api_device_code_url(device_code: "nonexistent"), as: :json

      assert_response :bad_request
      json = JSON.parse(response.body)
      assert_equal "invalid_grant", json["error"]
    end
  end
end
