# frozen_string_literal: true

require "test_helper"

class Hubs::ConnectionsControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:jason)
    @hub = hubs(:active_hub)
    sign_in @user
  end

  test "returns 403 when server_assisted_pairing is disabled" do
    @user.update!(server_assisted_pairing: false)

    get hub_connection_path(@hub)

    assert_response :forbidden
    json = JSON.parse(response.body)
    assert_equal "Server-assisted pairing is disabled", json["error"]
    assert json["message"].include?("QR code")
    assert json["secure_connect_url"].present?
    assert json["enable_convenience_url"].present?
  end

  test "returns device info when server_assisted_pairing is enabled" do
    @user.update!(server_assisted_pairing: true)

    get hub_connection_path(@hub)

    assert_response :success
    json = JSON.parse(response.body)
    assert_equal @hub.id, json["hub_id"]
    assert_equal @hub.identifier, json["identifier"]
    assert json["server_assisted_pairing"]
    assert json["device"].present?
  end

  test "returns 422 when hub has no device" do
    @user.update!(server_assisted_pairing: true)
    hub_without_device = Hub.create!(user: @user, identifier: "no-device-hub", last_seen_at: Time.current)

    get hub_connection_path(hub_without_device)

    assert_response :unprocessable_entity
    json = JSON.parse(response.body)
    assert_equal "Hub has no registered device for E2E encryption", json["error"]
  ensure
    hub_without_device&.destroy
  end

  test "returns 404 for nonexistent hub" do
    @user.update!(server_assisted_pairing: true)

    get hub_connection_path(hub_id: 999_999)

    assert_response :not_found
  end

  test "returns 404 for hub owned by another user" do
    @user.update!(server_assisted_pairing: true)
    other_user = users(:one)
    other_hub = Hub.create!(user: other_user, identifier: "other-hub", last_seen_at: Time.current)

    get hub_connection_path(other_hub)

    assert_response :not_found
  ensure
    other_hub&.destroy
  end

  test "requires authentication" do
    sign_out @user

    get hub_connection_path(@hub)

    assert_response :redirect
  end
end
