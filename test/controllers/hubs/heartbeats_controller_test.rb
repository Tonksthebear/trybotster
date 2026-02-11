# frozen_string_literal: true

require "test_helper"

class Hubs::HeartbeatsControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:jason)
    @hub = hubs(:active_hub)

    # Create a device + token dynamically so encrypted token lookup works
    @device = @user.devices.create!(
      name: "Heartbeat Test CLI",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8).scan(/../).join(":")
    )
    @device_token = @device.create_device_token!(name: "Heartbeat Test Token")

    # Associate the hub with this device so set_hub can find it
    @hub.update!(device: @device)
  end

  teardown do
    @device_token&.destroy
    @device&.destroy
  end

  # --- Successful heartbeat via Bearer token ---

  test "heartbeat with Bearer token updates last_seen_at" do
    old_last_seen = @hub.last_seen_at

    patch hub_heartbeat_path(@hub),
      headers: { "Authorization" => "Bearer #{@device_token.token}" },
      as: :json

    assert_response :success
    @hub.reload
    assert @hub.last_seen_at > old_last_seen, "last_seen_at should be updated"
  end

  test "heartbeat returns success JSON with last_seen_at" do
    patch hub_heartbeat_path(@hub),
      headers: { "Authorization" => "Bearer #{@device_token.token}" },
      as: :json

    assert_response :success
    json = JSON.parse(response.body)
    assert_equal true, json["success"]
    assert json["last_seen_at"].present?
  end

  # --- Authentication via X-API-Key header ---

  test "heartbeat with X-API-Key header authenticates successfully" do
    patch hub_heartbeat_path(@hub),
      headers: { "X-API-Key" => @device_token.token },
      as: :json

    assert_response :success
    json = JSON.parse(response.body)
    assert_equal true, json["success"]
  end

  # --- Authentication via Devise session ---

  test "heartbeat with session auth updates last_seen_at" do
    sign_in @user
    old_last_seen = @hub.last_seen_at

    patch hub_heartbeat_path(@hub), as: :json

    assert_response :success
    @hub.reload
    assert @hub.last_seen_at > old_last_seen
  end

  # --- Unauthenticated requests ---

  test "heartbeat without any credentials is rejected" do
    patch hub_heartbeat_path(@hub), as: :json

    assert_response :unauthorized
  end

  test "heartbeat with invalid API key returns unauthorized" do
    patch hub_heartbeat_path(@hub),
      headers: { "Authorization" => "Bearer btstr_completely_invalid_token" },
      as: :json

    assert_response :unauthorized
    json = JSON.parse(response.body)
    assert_equal "Invalid API key", json["error"]
  end

  # --- Hub not found / authorization boundary ---

  test "heartbeat returns not found for hub owned by another user" do
    other_user = users(:one)
    other_hub = Hub.create!(user: other_user, identifier: "other-heartbeat-hub", last_seen_at: Time.current)

    patch hub_heartbeat_path(other_hub),
      headers: { "Authorization" => "Bearer #{@device_token.token}" },
      as: :json

    assert_response :not_found
    json = JSON.parse(response.body)
    assert_equal "Hub not found", json["error"]
  ensure
    other_hub&.destroy
  end

  test "heartbeat returns not found for nonexistent hub ID" do
    sign_in @user

    patch hub_heartbeat_path(hub_id: 999_999), as: :json

    assert_response :not_found
  end

  # --- Device token usage tracking ---

  test "heartbeat via API key touches device token last_used_at" do
    assert_nil @device_token.last_used_at

    patch hub_heartbeat_path(@hub),
      headers: { "Authorization" => "Bearer #{@device_token.token}" },
      as: :json

    assert_response :success
    @device_token.reload
    assert_not_nil @device_token.last_used_at, "device token last_used_at should be set after auth"
  end
end
