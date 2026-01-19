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

  test "index shows list of active hubs in sidebar" do
    sign_in @user
    get hubs_path
    assert_response :success

    # Hubs are shown in sidebar - check for repo name in sidebar hub list
    assert_match /botster\/trybotster/, response.body

    # Main content shows "Select a Hub" message
    assert_select "h2", text: /Select a Hub/
  end

  test "index displays hub health indicators in sidebar" do
    sign_in @user
    get hubs_path
    assert_response :success

    # Active hub should have success indicator in sidebar
    assert_select ".bg-success-500"
  end

  test "index displays hub repo in sidebar" do
    sign_in @user
    get hubs_path
    assert_response :success

    # Should show repo name in sidebar
    assert_match /botster\/trybotster/, response.body
  end

  test "index links to hub show page" do
    sign_in @user
    get hubs_path
    assert_response :success

    # Should have link to hub show page
    assert_select "a[href=?]", hub_path(@active_hub)
  end

  test "index shows empty state when no hubs" do
    sign_in @user
    Hub.where(user: @user).destroy_all

    get hubs_path
    assert_response :success

    # Main content shows empty state
    assert_match /No Active Hubs/, response.body
    assert_match /botster-hub/, response.body
  end

  # === Show Tests ===

  test "show requires authentication" do
    get hub_path(@active_hub)
    assert_redirected_to root_path
  end

  test "show displays landing page with connection controller" do
    sign_in @user
    get hub_path(@active_hub)
    assert_response :success

    # Should have connection controller attached (permanent container for Turbo navigation)
    assert_select "[data-controller~='connection']"

    # Should pass hub ID to connection controller
    assert_select "[data-connection-hub-id-value=?]", @active_hub.id.to_s
  end

  test "show displays hub info" do
    sign_in @user
    get hub_path(@active_hub)
    assert_response :success

    # Should show hub identifier in header
    assert_match /hub-active-123/, response.body

    # Should show repo name
    assert_match /botster\/trybotster/, response.body
  end

  test "show has sidebar with hubs list" do
    sign_in @user
    get hub_path(@active_hub)
    assert_response :success

    # Sidebar contains link to hubs (the current hub should be highlighted)
    assert_select "[data-sidebar-hubs-target='list'] a[href=?]", hub_path(@active_hub)
  end

  test "show redirects to index for non-existent hub" do
    sign_in @user
    get hub_path("non-existent-hub-id")

    assert_redirected_to hubs_path
    assert_equal "Hub not found", flash[:alert]
  end

  test "show does not allow access to other users hubs" do
    other_user = User.create!(email: "other@example.com", username: "other")
    other_hub = Hub.create!(
      user: other_user,
      repo: "other/repo",
      identifier: "other-hub-id",
      last_seen_at: Time.current
    )

    sign_in @user
    get hub_path(other_hub)

    assert_redirected_to hubs_path
    assert_equal "Hub not found", flash[:alert]
  end

  test "show displays connection status indicator" do
    sign_in @user
    get hub_path(@active_hub)
    assert_response :success

    # Landing page has connection status in header (status text, not terminal badge)
    assert_select "[data-connection-target='status']"
    assert_select "[data-connection-target='statusText']"
  end

  test "show displays agents section" do
    sign_in @user
    get hub_path(@active_hub)
    assert_response :success

    # Landing page has agents section (JS populates after connection)
    assert_select "[data-agents-target='landingAgentList']"
  end

  test "show displays new agent button" do
    sign_in @user
    get hub_path(@active_hub)
    assert_response :success

    # Landing page has button to create new agent
    assert_select "[commandfor='new-agent-modal']"
  end
end
