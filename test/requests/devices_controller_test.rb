# frozen_string_literal: true

require "test_helper"

# Tests for device management endpoints.
#
# The CLI uses these endpoints to:
# 1. Register itself as a device (POST /devices)
# 2. Validate token / list devices (GET /devices)
#
# These tests verify the API contract the CLI expects.
class DevicesControllerTest < ActionDispatch::IntegrationTest
  include ApiTestHelper

  # Helper to generate unique fingerprints per test
  def unique_fingerprint(prefix = "test")
    "#{prefix}:#{SecureRandom.hex(7).scan(/../).join(':')}"
  end

  # ==========================================================================
  # GET /devices - List devices / validate token
  # ==========================================================================

  test "GET /devices returns 401 with invalid token" do
    get devices_url, headers: {
      "Authorization" => "Bearer invalid_token",
      "Accept" => "application/json"
    }

    assert_response :unauthorized
    assert_json_error("Invalid API key")
  end

  test "GET /devices returns device list with valid token" do
    get devices_url, headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_response

    assert_kind_of Array, json
  end

  test "GET /devices returns correct device fields" do
    # Create a device first
    users(:jason).devices.create!(
      device_type: "cli",
      name: "Test CLI Device",
      fingerprint: unique_fingerprint("fields")
    )

    get devices_url, headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_response

    assert_operator json.length, :>=, 1

    device = json.find { |d| d["name"] == "Test CLI Device" }
    assert_not_nil device, "Expected to find 'Test CLI Device' in response"
    assert device.key?("id")
    assert device.key?("name")
    assert device.key?("device_type")
    assert device.key?("fingerprint")
    assert device.key?("last_seen_at")
  end

  test "GET /devices only returns current user's devices" do
    # Create devices for different users
    users(:jason).devices.create!(device_type: "cli", name: "Jason Device", fingerprint: unique_fingerprint("jason"))
    users(:one).devices.create!(device_type: "cli", name: "Other User Device", fingerprint: unique_fingerprint("one"))

    get devices_url, headers: auth_headers_for(:jason)

    json = assert_json_response
    device_names = json.map { |d| d["name"] }

    assert_includes device_names, "Jason Device"
    refute_includes device_names, "Other User Device"
  end

  # ==========================================================================
  # POST /devices - Register device
  # ==========================================================================

  test "POST /devices returns 401 without authentication" do
    post devices_url,
      params: { device_type: "cli", name: "Test", fingerprint: unique_fingerprint }.to_json,
      headers: json_headers

    assert_response :unauthorized
  end

  test "POST /devices creates new device with fingerprint" do
    fingerprint = unique_fingerprint("create")
    # Pre-create auth headers so auth device isn't counted in assert_difference
    headers = auth_headers_for(:jason)

    assert_difference -> { Device.count }, 1 do
      post devices_url,
        params: {
          device_type: "cli",
          name: "My CLI Device",
          fingerprint: fingerprint
        }.to_json,
        headers: headers
    end

    assert_response :created
    json = assert_json_keys(:device_id, :fingerprint, :created)

    assert_kind_of Integer, json["device_id"]
    assert_equal fingerprint, json["fingerprint"]
    assert_equal true, json["created"]
  end

  test "POST /devices returns existing device if fingerprint matches" do
    fingerprint = unique_fingerprint("existing")
    # Pre-create auth headers so auth device isn't counted in assert_difference
    headers = auth_headers_for(:jason)

    # Create device first
    existing = users(:jason).devices.create!(
      device_type: "cli",
      name: "Existing Device",
      fingerprint: fingerprint
    )

    assert_no_difference -> { Device.count } do
      post devices_url,
        params: {
          device_type: "cli",
          name: "Updated Name",
          fingerprint: fingerprint
        }.to_json,
        headers: headers
    end

    assert_response :ok  # Not 201 since not newly created
    json = assert_json_keys(:device_id, :fingerprint, :created)

    assert_equal existing.id, json["device_id"]
    assert_equal false, json["created"]
  end

  test "POST /devices requires device_type" do
    post devices_url,
      params: {
        name: "Test Device",
        fingerprint: unique_fingerprint
      }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :bad_request
  end

  test "POST /devices requires name" do
    post devices_url,
      params: {
        device_type: "cli",
        fingerprint: unique_fingerprint
      }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :bad_request
  end

  test "POST /devices requires fingerprint or public_key" do
    post devices_url,
      params: {
        device_type: "cli",
        name: "Test Device"
      }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :bad_request
    assert_json_error(/fingerprint or public_key/i)
  end

  test "POST /devices can register browser device with public_key" do
    post devices_url,
      params: {
        device_type: "browser",
        name: "Browser Session",
        public_key: "base64_encoded_public_key_#{SecureRandom.hex(8)}"
      }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :created
    json = assert_json_keys(:device_id, :created, :server_assisted_pairing)

    assert_equal true, json["server_assisted_pairing"]
  end

  # ==========================================================================
  # DELETE /devices/:id - Remove device
  # ==========================================================================

  test "DELETE /devices/:id removes device" do
    # Pre-create auth headers so auth device isn't counted in assert_difference
    headers = auth_headers_for(:jason)

    device = users(:jason).devices.create!(
      device_type: "cli",
      name: "Device to Delete",
      fingerprint: unique_fingerprint("delete")
    )

    assert_difference -> { Device.count }, -1 do
      delete device_url(device), headers: headers
    end

    assert_response :no_content
  end

  test "DELETE /devices/:id returns 404 for other user's device" do
    # Pre-create auth headers so auth device isn't counted in assert_difference
    headers = auth_headers_for(:jason)

    other_device = users(:one).devices.create!(
      device_type: "cli",
      name: "Other User Device",
      fingerprint: unique_fingerprint("other")
    )

    assert_no_difference -> { Device.count } do
      delete device_url(other_device), headers: headers
    end

    assert_response :not_found
  end
end
