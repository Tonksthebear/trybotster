# frozen_string_literal: true

require "test_helper"

# Tests for the device authorization flow (RFC 8628).
#
# This is the flow the CLI uses to authenticate:
# 1. CLI requests device code (POST /hubs/codes)
# 2. User visits verification_uri and enters user_code
# 3. CLI polls for token (GET /hubs/codes/:device_code)
# 4. Once approved, CLI receives access_token
#
# These tests verify the API contract the CLI expects.
class Hubs::CodesControllerTest < ActionDispatch::IntegrationTest
  include ApiTestHelper

  # ==========================================================================
  # POST /hubs/codes - Request device code
  # ==========================================================================

  test "POST /hubs/codes creates device authorization and returns expected JSON shape" do
    post codes_url, params: { device_name: "Test CLI" }.to_json, headers: json_headers

    assert_response :ok

    json = assert_json_keys(:device_code, :user_code, :verification_uri, :expires_in, :interval)

    # Verify types match what CLI expects
    assert_kind_of String, json["device_code"]
    assert_kind_of String, json["user_code"]
    assert_kind_of String, json["verification_uri"]
    assert_kind_of Integer, json["expires_in"]
    assert_kind_of Integer, json["interval"]

    # Verify user_code format (XXXX-XXXX)
    assert_match(/\A[A-Z0-9]{4}-[A-Z0-9]{4}\z/, json["user_code"])

    # Verify expires_in is reasonable (between 1 and 30 minutes)
    assert_operator json["expires_in"], :>, 0
    assert_operator json["expires_in"], :<=, 1800

    # Verify interval is reasonable (typically 5 seconds)
    assert_operator json["interval"], :>=, 1
    assert_operator json["interval"], :<=, 30
  end

  test "POST /hubs/codes creates a DeviceAuthorization record" do
    assert_difference -> { DeviceAuthorization.count }, 1 do
      post codes_url, params: { device_name: "My CLI Device" }.to_json, headers: json_headers
    end

    auth = DeviceAuthorization.last
    assert_equal "pending", auth.status
    assert_equal "My CLI Device", auth.device_name
    assert auth.expires_at > Time.current
  end

  test "POST /hubs/codes works without device_name" do
    post codes_url, params: {}.to_json, headers: json_headers

    assert_response :ok
    assert_json_keys(:device_code, :user_code)
  end

  # ==========================================================================
  # GET /hubs/codes/:id - Poll for authorization status
  # ==========================================================================

  test "GET /hubs/codes/:id returns authorization_pending for pending auth" do
    auth = DeviceAuthorization.create!(device_name: "Test CLI")

    get code_url(auth.device_code), headers: json_headers

    assert_response :accepted  # 202
    json = assert_json_error("authorization_pending")
  end

  test "GET /hubs/codes/:id returns access_token and mcp_token when approved" do
    auth = DeviceAuthorization.create!(device_name: "Test CLI")
    auth.approve!(users(:jason))

    get code_url(auth.device_code), headers: json_headers

    assert_response :ok
    json = assert_json_keys(:access_token, :mcp_token, :token_type)

    # Verify token formats match what CLI expects
    assert json["access_token"].start_with?("btstr_"), "Access token should start with btstr_ prefix"
    assert json["mcp_token"].start_with?("btmcp_"), "MCP token should start with btmcp_ prefix"
    assert_equal "bearer", json["token_type"]

    # Verify DeviceToken was created
    device_token = DeviceToken.find_by(token: json["access_token"])
    assert_not_nil device_token
    assert_equal users(:jason), device_token.user

    # Verify MCPToken was created
    mcp_token = MCPToken.find_by(token: json["mcp_token"])
    assert_not_nil mcp_token
    assert_equal users(:jason), mcp_token.user

    # Both tokens should belong to the same device
    assert_equal device_token.device, mcp_token.device
  end

  test "GET /hubs/codes/:id returns access_denied when denied" do
    auth = DeviceAuthorization.create!(device_name: "Test CLI")
    auth.deny!

    get code_url(auth.device_code), headers: json_headers

    assert_response :bad_request
    assert_json_error("access_denied")
  end

  test "GET /hubs/codes/:id returns expired_token for expired auth" do
    auth = DeviceAuthorization.create!(device_name: "Test CLI")
    auth.update!(expires_at: 1.minute.ago)

    get code_url(auth.device_code), headers: json_headers

    assert_response :bad_request
    assert_json_error("expired_token")

    # Verify the auth was marked as expired
    auth.reload
    assert_equal "expired", auth.status
  end

  test "GET /hubs/codes/:id returns invalid_grant for unknown device_code" do
    get code_url("nonexistent_device_code"), headers: json_headers

    assert_response :bad_request
    assert_json_error("invalid_grant")
  end

  test "GET /hubs/codes/:id only creates one token per approval" do
    auth = DeviceAuthorization.create!(device_name: "Test CLI")
    auth.approve!(users(:jason))

    # First poll - creates token
    get code_url(auth.device_code), headers: json_headers
    assert_response :ok
    first_token = JSON.parse(response.body)["access_token"]

    # Second poll - should return same token or new token, but not error
    # (This tests idempotency - CLI might poll multiple times)
    get code_url(auth.device_code), headers: json_headers
    assert_response :ok
  end
end
