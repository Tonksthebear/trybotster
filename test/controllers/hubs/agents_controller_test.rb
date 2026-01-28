# frozen_string_literal: true

require "test_helper"

module Hubs
  class AgentsControllerTest < ActionDispatch::IntegrationTest
    include Devise::Test::IntegrationHelpers

    setup do
      @user = users(:jason)
      @active_hub = hubs(:active_hub)
    end

    # === Authentication Tests ===

    test "show requires authentication" do
      get hub_agent_path(@active_hub, 0)
      assert_redirected_to root_path
    end

    # === Show Tests ===

    test "show displays terminal view for agent" do
      sign_in @user
      get hub_agent_path(@active_hub, 0)
      assert_response :success

      # Should have hub-connection controller attached (permanent container)
      assert_select "[data-controller~='hub-connection']"

      # Should pass hub ID to hub-connection controller
      assert_select "[data-hub-connection-hub-id-value=?]", @active_hub.id.to_s

      # Should have terminal-connection controller attached
      assert_select "[data-controller~='terminal-connection']"

      # Should pass hub ID to terminal-connection controller
      assert_select "[data-terminal-connection-hub-id-value=?]", @active_hub.id.to_s
    end

    test "show displays terminal elements" do
      sign_in @user
      get hub_agent_path(@active_hub, 0)
      assert_response :success

      # Terminal view has terminal badge and security banner
      assert_select "[data-terminal-connection-target='terminalBadge']"
      assert_select "[data-hub-connection-target='securityBanner']"
    end

    test "show displays terminal display controller" do
      sign_in @user
      get hub_agent_path(@active_hub, 0)
      assert_response :success

      # Should have terminal-display controller
      assert_select "[data-controller~='terminal-display']"
    end

    test "show displays hub info in header" do
      sign_in @user
      get hub_agent_path(@active_hub, 0)
      assert_response :success

      # Should show hub identifier
      assert_match /hub-active-123/, response.body
    end

    test "show redirects to hubs for non-existent hub" do
      sign_in @user
      get hub_agent_path("non-existent-hub-id", 0)

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
      get hub_agent_path(other_hub, 0)

      assert_redirected_to hubs_path
      assert_equal "Hub not found", flash[:alert]
    end

    test "show displays sidebar with hubs list" do
      sign_in @user
      get hub_agent_path(@active_hub, 0)
      assert_response :success

      # Sidebar contains link to hub landing page
      assert_select "[data-sidebar-hubs-target='list'] a[href=?]", hub_path(@active_hub)
    end
  end
end
