# frozen_string_literal: true

require "test_helper"

# Tests for browser key management endpoints.
#
# The DevicesController now manages BrowserKey records:
# 1. Register a browser key (POST /devices with device_type: "browser")
# 2. List browser keys (GET /devices)
# 3. Delete a browser key (DELETE /devices/:id)
#
# These tests verify the API contract.
class DevicesControllerTest < ActionDispatch::IntegrationTest
  include ApiTestHelper

  # ==========================================================================
  # GET /devices - List browser keys / validate token
  # ==========================================================================

  test "GET /devices returns 401 with invalid token" do
    get devices_url, headers: {
      "Authorization" => "Bearer invalid_token",
      "Accept" => "application/json"
    }

    assert_response :unauthorized
    assert_json_error("Invalid API key")
  end

  test "GET /devices returns browser key list with valid token" do
    get devices_url, headers: auth_headers_for(:primary_user)

    assert_response :ok
    json = assert_json_response

    assert_kind_of Array, json
  end

  test "GET /devices returns correct browser key fields" do
    # Create a browser key first
    users(:primary_user).browser_keys.create!(
      name: "Test Browser",
      public_key: "test_browser_pk_#{SecureRandom.hex(8)}",
      fingerprint: "te:st:#{SecureRandom.hex(6).scan(/../).join(':')}"
    )

    get devices_url, headers: auth_headers_for(:primary_user)

    assert_response :ok
    json = assert_json_response

    assert_operator json.length, :>=, 1

    browser_key = json.find { |d| d["name"] == "Test Browser" }
    assert_not_nil browser_key, "Expected to find 'Test Browser' in response"
    assert browser_key.key?("id")
    assert browser_key.key?("name")
    assert browser_key.key?("device_type")
    assert browser_key.key?("fingerprint")
    assert browser_key.key?("last_seen_at")
  end

  test "GET /devices only returns current user's browser keys" do
    users(:primary_user).browser_keys.create!(
      name: "Primary Browser",
      public_key: "primary_bk_#{SecureRandom.hex(8)}",
      fingerprint: "ja:#{SecureRandom.hex(7).scan(/../).join(':')}"
    )
    users(:one).browser_keys.create!(
      name: "Other User Browser",
      public_key: "other_bk_#{SecureRandom.hex(8)}",
      fingerprint: "ot:#{SecureRandom.hex(7).scan(/../).join(':')}"
    )

    get devices_url, headers: auth_headers_for(:primary_user)

    json = assert_json_response
    names = json.map { |d| d["name"] }

    assert_includes names, "Primary Browser"
    refute_includes names, "Other User Browser"
  end

  # ==========================================================================
  # POST /devices - Register browser key
  # ==========================================================================

  test "POST /devices returns 401 without authentication" do
    post devices_url,
      params: { device_type: "browser", name: "Test", public_key: "pk_test" }.to_json,
      headers: json_headers

    assert_response :unauthorized
  end

  test "POST /devices can register browser key with public_key" do
    post devices_url,
      params: {
        device_type: "browser",
        name: "Browser Session",
        public_key: "base64_encoded_public_key_#{SecureRandom.hex(8)}"
      }.to_json,
      headers: auth_headers_for(:primary_user)

    assert_response :created
    json = assert_json_keys(:device_id, :created)
  end

  test "POST /devices requires browser device_type" do
    post devices_url,
      params: {
        device_type: "cli",
        name: "CLI Device",
        fingerprint: "te:st:00:11:22:33:44:55"
      }.to_json,
      headers: auth_headers_for(:primary_user)

    assert_response :bad_request
  end

  test "POST /devices requires public_key for browser" do
    post devices_url,
      params: {
        device_type: "browser",
        name: "Browser"
      }.to_json,
      headers: auth_headers_for(:primary_user)

    assert_response :bad_request
  end

  # ==========================================================================
  # DELETE /devices/:id - Remove browser key
  # ==========================================================================

  test "DELETE /devices/:id removes browser key" do
    headers = auth_headers_for(:primary_user)

    browser_key = users(:primary_user).browser_keys.create!(
      name: "Browser to Delete",
      public_key: "delete_pk_#{SecureRandom.hex(8)}",
      fingerprint: "de:#{SecureRandom.hex(7).scan(/../).join(':')}"
    )

    assert_difference -> { BrowserKey.count }, -1 do
      delete device_url(browser_key), headers: headers
    end

    assert_response :no_content
  end

  test "DELETE /devices/:id returns 404 for other user's browser key" do
    headers = auth_headers_for(:primary_user)

    other_key = users(:one).browser_keys.create!(
      name: "Other User Browser",
      public_key: "other_pk_#{SecureRandom.hex(8)}",
      fingerprint: "ot:#{SecureRandom.hex(7).scan(/../).join(':')}"
    )

    assert_no_difference -> { BrowserKey.count } do
      delete device_url(other_key), headers: headers
    end

    assert_response :not_found
  end
end
