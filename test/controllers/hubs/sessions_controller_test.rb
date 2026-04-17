# frozen_string_literal: true

require "test_helper"

module Hubs
  class SessionsControllerTest < ActionDispatch::IntegrationTest
    include Devise::Test::IntegrationHelpers

    setup do
      @user = users(:primary_user)
      @active_hub = hubs(:active_hub)
    end

    # === Authentication ===

    test "show requires authentication" do
      get hub_session_path(@active_hub, "test-session-uuid")
      assert_redirected_to root_path
    end

    # === Session View ===

    test "show serves SPA shell" do
      sign_in @user
      get hub_session_path(@active_hub, "test-session-uuid")

      assert_response :success
      assert_select "#app"
    end

    # === Hub Scoping ===

    test "show redirects to hubs for non-existent hub" do
      sign_in @user
      get hub_session_path("non-existent-hub-id", "test-session-uuid")

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
      get hub_session_path(other_hub, "test-session-uuid")

      assert_redirected_to hubs_path
      assert_equal "Hub not found", flash[:alert]
    ensure
      other_hub&.destroy
    end
  end
end
