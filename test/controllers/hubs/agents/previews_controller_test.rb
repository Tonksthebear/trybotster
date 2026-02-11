# frozen_string_literal: true

require "test_helper"

class Hubs::Agents::PreviewsControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:jason)
    @hub = hubs(:active_hub)
    sign_in @user
  end

  # === Bootstrap ===

  test "bootstrap renders without layout" do
    get hub_agent_pty_preview_path(@hub, 0, 1)

    assert_response :success
    # Should include service worker registration
    assert_match "sw.js", response.body
  end

  test "bootstrap sets correct scope path with agent and pty indices" do
    get hub_agent_pty_preview_path(@hub, 2, 3)

    assert_response :success
    # Scope path must include the correct agent_index and pty_index
    assert_match "/hubs/#{@hub.id}/agents/2/3/preview", response.body
  end

  # === Shell ===

  test "shell renders without layout" do
    get hub_agent_pty_preview_shell_path(@hub, 0, 1)

    assert_response :success
  end

  # === Service Worker ===

  test "service worker returns javascript content type" do
    get hub_agent_pty_service_worker_path(@hub, 0, 1)

    assert_response :success
    assert_equal "application/javascript", response.headers["Content-Type"]
  end

  test "service worker sets Service-Worker-Allowed header to scope" do
    get hub_agent_pty_service_worker_path(@hub, 0, 1)

    assert_response :success
    assert_equal "/hubs/#{@hub.id}/agents/0/1/preview", response.headers["Service-Worker-Allowed"]
  end

  # === Auth & Hub Scoping ===

  test "requires authentication" do
    sign_out @user

    get hub_agent_pty_preview_path(@hub, 0, 1)

    assert_response :redirect
  end

  test "redirects when hub belongs to another user" do
    other_user = users(:one)
    other_hub = Hub.create!(user: other_user, identifier: "other-hub", last_seen_at: Time.current)

    get hub_agent_pty_preview_path(other_hub, 0, 1)

    assert_response :redirect
    assert_redirected_to hubs_path
  ensure
    other_hub&.destroy
  end

  test "redirects for nonexistent hub" do
    get hub_agent_pty_preview_path(hub_id: 999_999, agent_index: 0, pty_index: 1)

    assert_response :redirect
    assert_redirected_to hubs_path
  end
end
