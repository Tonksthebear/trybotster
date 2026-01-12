# frozen_string_literal: true

require "test_helper"

# Tests for hub lifecycle endpoints.
#
# The CLI uses these endpoints to:
# 1. Register/update hub on startup (PUT /hubs/:identifier)
# 2. Heartbeat to keep hub alive (PATCH /hubs/:identifier/heartbeat)
# 3. Unregister hub on shutdown (DELETE /hubs/:identifier)
#
# These tests verify the API contract the CLI expects.
class HubsControllerTest < ActionDispatch::IntegrationTest
  include ApiTestHelper

  # ==========================================================================
  # PUT /hubs/:identifier - Register/Update Hub
  # ==========================================================================

  test "PUT /hubs/:identifier returns 401 without authentication" do
    put hub_url("new-hub-123"),
      params: { repo: "owner/repo" }.to_json,
      headers: json_headers

    assert_response :unauthorized
  end

  test "PUT /hubs/:identifier creates new hub" do
    identifier = "new-hub-#{SecureRandom.hex(8)}"

    assert_difference -> { Hub.count }, 1 do
      put hub_url(identifier),
        params: { repo: "owner/repo" }.to_json,
        headers: auth_headers_for(:jason)
    end

    assert_response :ok
    json = assert_json_keys(:success, :hub_id)

    assert_equal true, json["success"]
    assert_kind_of Integer, json["hub_id"]

    hub = Hub.find(json["hub_id"])
    assert_equal identifier, hub.identifier
    assert_equal "owner/repo", hub.repo
    assert_equal users(:jason), hub.user
  end

  test "PUT /hubs/:identifier updates existing hub" do
    hub = hubs(:active_hub)
    original_id = hub.id

    assert_no_difference -> { Hub.count } do
      put hub_url(hub.identifier),
        params: { repo: "updated/repo" }.to_json,
        headers: auth_headers_for(:jason)
    end

    assert_response :ok
    json = assert_json_keys(:success, :hub_id)

    assert_equal original_id, json["hub_id"]

    hub.reload
    assert_equal "updated/repo", hub.repo
  end

  test "PUT /hubs/:identifier updates last_seen_at" do
    hub = hubs(:stale_hub)
    old_last_seen = hub.last_seen_at

    put hub_url(hub.identifier),
      params: { repo: hub.repo }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :ok

    hub.reload
    assert_operator hub.last_seen_at, :>, old_last_seen
  end

  test "PUT /hubs/:identifier syncs agents" do
    hub = hubs(:active_hub)

    put hub_url(hub.identifier),
      params: {
        repo: hub.repo,
        agents: [
          { session_key: "session-123", last_invocation_url: "https://github.com/owner/repo/issues/42" },
          { session_key: "session-456" }
        ]
      }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :ok

    hub.reload
    assert_equal 2, hub.hub_agents.count
  end

  test "PUT /hubs/:identifier associates device when device_id provided" do
    device = users(:jason).devices.create!(
      device_type: "cli",
      name: "My CLI",
      fingerprint: "hub:test:#{SecureRandom.hex(6)}"
    )

    identifier = "hub-with-device-#{SecureRandom.hex(4)}"

    put hub_url(identifier),
      params: { repo: "owner/repo", device_id: device.id }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :ok

    hub = Hub.find_by(identifier: identifier)
    assert_equal device, hub.device
  end

  test "PUT /hubs/:identifier returns e2e_enabled status" do
    put hub_url("hub-e2e-test"),
      params: { repo: "owner/repo" }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_response

    assert json.key?("e2e_enabled")
  end

  # ==========================================================================
  # PATCH /hubs/:identifier/heartbeat - Heartbeat
  # ==========================================================================

  test "PATCH /hubs/:identifier/heartbeat returns 401 without authentication" do
    hub = hubs(:active_hub)

    patch hub_heartbeat_url(hub.identifier),
      headers: json_headers

    assert_response :unauthorized
  end

  test "PATCH /hubs/:identifier/heartbeat updates last_seen_at" do
    hub = hubs(:stale_hub)
    old_last_seen = hub.last_seen_at

    patch hub_heartbeat_url(hub.identifier),
      headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_keys(:success, :last_seen_at)

    assert_equal true, json["success"]

    hub.reload
    assert_operator hub.last_seen_at, :>, old_last_seen
  end

  test "PATCH /hubs/:identifier/heartbeat returns 404 for unknown hub" do
    patch hub_heartbeat_url("nonexistent-hub"),
      headers: auth_headers_for(:jason)

    assert_response :not_found
    assert_json_error("Hub not found")
  end

  test "PATCH /hubs/:identifier/heartbeat returns 404 for other user's hub" do
    # Create hub for user :one
    other_hub = users(:one).hubs.create!(
      identifier: "other-user-hub-#{SecureRandom.hex(4)}",
      repo: "other/repo",
      last_seen_at: Time.current
    )

    # Try to heartbeat with jason's token
    patch hub_heartbeat_url(other_hub.identifier),
      headers: auth_headers_for(:jason)

    assert_response :not_found
  end

  # ==========================================================================
  # DELETE /hubs/:identifier - Unregister Hub
  # ==========================================================================

  test "DELETE /hubs/:identifier returns 401 without authentication" do
    hub = hubs(:active_hub)

    delete hub_url(hub.identifier),
      headers: json_headers

    assert_response :unauthorized
  end

  test "DELETE /hubs/:identifier removes hub" do
    hub = hubs(:active_hub)

    assert_difference -> { Hub.count }, -1 do
      delete hub_url(hub.identifier),
        headers: auth_headers_for(:jason)
    end

    assert_response :ok
    json = assert_json_keys(:success)

    assert_equal true, json["success"]
    assert_nil Hub.find_by(id: hub.id)
  end

  test "DELETE /hubs/:identifier is idempotent for nonexistent hub" do
    delete hub_url("already-gone-hub"),
      headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_keys(:success)

    assert_equal true, json["success"]
  end

  test "DELETE /hubs/:identifier cannot delete other user's hub" do
    other_hub = users(:one).hubs.create!(
      identifier: "other-hub-#{SecureRandom.hex(4)}",
      repo: "other/repo",
      last_seen_at: Time.current
    )

    assert_no_difference -> { Hub.count } do
      delete hub_url(other_hub.identifier),
        headers: auth_headers_for(:jason)
    end

    # Returns success because it's idempotent (hub "not found" for this user)
    assert_response :ok
  end
end
