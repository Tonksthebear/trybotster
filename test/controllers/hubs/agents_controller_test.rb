# frozen_string_literal: true

require "test_helper"

module Hubs
  class AgentsControllerTest < ActionDispatch::IntegrationTest
    include Devise::Test::IntegrationHelpers

    setup do
      @user = users(:jason)
      @active_hub = hubs(:active_hub)
    end

    # === Authentication ===

    test "show requires authentication" do
      get hub_agent_path(@active_hub, 0)
      assert_redirected_to root_path
    end

    # === Redirect Behavior ===

    test "show redirects to PTY 0 for agent" do
      sign_in @user
      get hub_agent_path(@active_hub, 0)

      assert_redirected_to hub_agent_pty_path(@active_hub, 0, 0)
    end

    test "show preserves agent index in redirect" do
      sign_in @user
      get hub_agent_path(@active_hub, 2)

      assert_redirected_to hub_agent_pty_path(@active_hub, 2, 0)
    end

    # === Hub Scoping ===

    test "show redirects to hubs for non-existent hub" do
      sign_in @user
      get hub_agent_path("non-existent-hub-id", 0)

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
      get hub_agent_path(other_hub, 0)

      assert_redirected_to hubs_path
      assert_equal "Hub not found", flash[:alert]
    ensure
      other_hub&.destroy
    end
  end
end
