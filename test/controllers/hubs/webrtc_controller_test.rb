# frozen_string_literal: true

require "test_helper"

class Hubs::WebrtcControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers
  include ApiTestHelper

  setup do
    @user = users(:jason)
    @hub = hubs(:active_hub)
  end

  # === Browser Auth (session) ===

  test "returns ICE servers for authenticated browser user" do
    sign_in @user

    # Clear metered config so we don't hit external API
    ENV.delete("METERED_DOMAIN")
    ENV.delete("METERED_SECRET_KEY")

    get hub_webrtc_path(@hub)

    assert_response :success
    json = JSON.parse(response.body)
    assert json["ice_servers"].is_a?(Array)
    assert json["ice_servers"].any? { |s| s["urls"]&.include?("stun:") }
  end

  # === CLI Auth (Bearer token) ===

  test "returns ICE servers for CLI with device token" do
    ENV.delete("METERED_DOMAIN")
    ENV.delete("METERED_SECRET_KEY")

    get hub_webrtc_path(@hub), headers: auth_headers_for(:jason)

    assert_response :success
    json = JSON.parse(response.body)
    assert json["ice_servers"].is_a?(Array)
  end

  test "returns 401 without authentication" do
    get hub_webrtc_path(@hub), headers: { "Content-Type" => "application/json" }

    assert_response :unauthorized
  end

  test "returns 401 with invalid token" do
    get hub_webrtc_path(@hub), headers: {
      "Authorization" => "Bearer btstr_invalid_token",
      "Content-Type" => "application/json"
    }

    assert_response :unauthorized
  end

  test "returns 404 for nonexistent hub" do
    sign_in @user

    get hub_webrtc_path(hub_id: 999_999)

    assert_response :not_found
  end

  test "returns 404 for hub owned by another user" do
    sign_in @user
    other_user = users(:one)
    other_hub = Hub.create!(user: other_user, identifier: "other-hub", last_seen_at: Time.current)

    get hub_webrtc_path(other_hub)

    assert_response :not_found
  ensure
    other_hub&.destroy
  end

  # === TURN Credentials ===

  test "includes TURN credentials when TURN_SECRET is configured" do
    sign_in @user

    ENV.delete("METERED_DOMAIN")
    ENV.delete("METERED_SECRET_KEY")
    ENV["TURN_SERVER_URL"] = "turn:turn.example.com:3478"
    ENV["TURN_SECRET"] = "test_secret"

    get hub_webrtc_path(@hub)

    assert_response :success
    json = JSON.parse(response.body)

    turn_servers = json["ice_servers"].select { |s| s["urls"]&.include?("turn:") }
    assert turn_servers.any?, "Should include TURN servers"

    turn = turn_servers.first
    assert turn["username"].present?
    assert turn["credential"].present?
    # Username format: timestamp:hub_id
    assert_match(/\d+:#{@hub.id}/, turn["username"])
  ensure
    ENV.delete("TURN_SERVER_URL")
    ENV.delete("TURN_SECRET")
  end

  test "returns only STUN when no TURN config" do
    sign_in @user

    ENV.delete("TURN_SERVER_URL")
    ENV.delete("TURN_SECRET")
    ENV.delete("METERED_DOMAIN")
    ENV.delete("METERED_SECRET_KEY")

    get hub_webrtc_path(@hub)

    assert_response :success
    json = JSON.parse(response.body)

    turn_servers = json["ice_servers"].select { |s| s["urls"]&.include?("turn:") }
    assert_empty turn_servers, "Should not include TURN servers without config"
  end
end
