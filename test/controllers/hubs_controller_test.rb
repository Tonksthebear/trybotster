# frozen_string_literal: true

require "test_helper"

class HubsControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:jason)
    @active_hub = hubs(:active_hub)
    @stale_hub = hubs(:stale_hub)
  end

  # === Index Tests ===

  test "index requires authentication" do
    get hubs_path
    assert_redirected_to root_path
  end

  test "index shows select a hub message when hubs exist" do
    sign_in @user
    get hubs_path
    assert_response :success

    assert_select "h2", text: /Select a Hub/
  end

  test "index shows empty state when no hubs" do
    sign_in @user
    Hub.where(user: @user).destroy_all

    get hubs_path
    assert_response :success

    assert_select "h2", text: /No Active Hubs/
    assert_match "botster-hub", response.body
  end

  # === Show HTML Tests ===

  test "show requires authentication" do
    get hub_path(@active_hub)
    assert_redirected_to root_path
  end

  test "show displays hub info" do
    sign_in @user
    get hub_path(@active_hub)
    assert_response :success

    # Should show hub identifier
    assert_match @active_hub.identifier, response.body
  end

  test "show displays agents section with agent-list controller" do
    sign_in @user
    get hub_path(@active_hub)
    assert_response :success

    assert_select "[data-controller='agent-list']"
  end

  test "show displays new agent button" do
    sign_in @user
    get hub_path(@active_hub)
    assert_response :success

    assert_select "[commandfor='new-agent-modal']"
  end

  test "show displays settings link" do
    sign_in @user
    get hub_path(@active_hub)
    assert_response :success

    assert_select "a[href=?]", hub_settings_path(@active_hub)
  end

  test "show redirects to index for non-existent hub" do
    sign_in @user
    get hub_path("non-existent-hub-id")

    assert_redirected_to hubs_path
    assert_equal "Hub not found", flash[:alert]
  end

  test "show does not allow access to other users hubs" do
    other_user = users(:one)
    other_hub = Hub.create!(
      user: other_user,
      identifier: "other-hub-id",
      last_seen_at: Time.current
    )

    sign_in @user
    get hub_path(other_hub)

    assert_redirected_to hubs_path
    assert_equal "Hub not found", flash[:alert]
  ensure
    other_hub&.destroy
  end

  # === JSON Status Endpoint Tests ===

  test "show json returns hub status for active hub" do
    sign_in @user
    get hub_path(@active_hub), as: :json
    assert_response :success

    json = JSON.parse(response.body)
    assert_equal @active_hub.id, json["id"]
    assert_equal @active_hub.identifier, json["identifier"]
    assert json["active"], "Expected active hub to be marked active"
    assert json["alive"], "Expected active hub to have alive=true"
    assert_not_nil json["last_seen_at"]
    assert_not_nil json["seconds_since_heartbeat"]
    assert json["seconds_since_heartbeat"] < 120, "Active hub should have recent heartbeat"
  end

  test "show json returns hub status for stale hub" do
    sign_in @user
    get hub_path(@stale_hub), as: :json
    assert_response :success

    json = JSON.parse(response.body)
    assert_equal @stale_hub.id, json["id"]
    assert_not json["active"], "Expected stale hub to be marked inactive"
    assert_not json["alive"], "Expected stale hub to have alive=false"
    assert json["seconds_since_heartbeat"] > 120, "Stale hub should have old heartbeat"
  end

  test "show json returns 404 for non-existent hub" do
    sign_in @user
    get hub_path("non-existent"), as: :json
    assert_response :not_found

    json = JSON.parse(response.body)
    assert_equal "Hub not found", json["error"]
  end

  test "show json requires authentication" do
    get hub_path(@active_hub), as: :json
    assert_response :unauthorized
  end
end
