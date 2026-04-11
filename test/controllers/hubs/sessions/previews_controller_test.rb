# frozen_string_literal: true

require "test_helper"

class Hubs::Sessions::PreviewsControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:primary_user)
    @hub = hubs(:active_hub)
    @session_uuid = "test-preview-session-uuid"
    sign_in @user
  end

  # === Bootstrap ===

  test "bootstrap renders without layout" do
    get hub_session_preview_path(@hub, @session_uuid)

    assert_response :success
    assert_match "sw.js", response.body
  end

  test "bootstrap sets correct scope path with session uuid" do
    get hub_session_preview_path(@hub, @session_uuid)

    assert_response :success
    assert_match "/hubs/#{@hub.id}/sessions/#{@session_uuid}/preview", response.body
  end

  # === Shell ===

  test "shell renders without layout" do
    get hub_session_preview_shell_path(@hub, @session_uuid)

    assert_response :success
  end

  # === Service Worker ===

  test "service worker returns javascript content type" do
    get hub_session_preview_service_worker_path(@hub, @session_uuid)

    assert_response :success
    assert_equal "application/javascript", response.headers["Content-Type"]
  end

  test "service worker sets Service-Worker-Allowed header to scope" do
    get hub_session_preview_service_worker_path(@hub, @session_uuid)

    assert_response :success
    assert_equal "/hubs/#{@hub.id}/sessions/#{@session_uuid}/preview", response.headers["Service-Worker-Allowed"]
  end

  # === Auth & Hub Scoping ===

  test "requires authentication" do
    sign_out @user

    get hub_session_preview_path(@hub, @session_uuid)

    assert_response :redirect
  end

  test "redirects when hub belongs to another user" do
    other_user = users(:one)
    other_hub = Hub.create!(user: other_user, identifier: "other-hub", last_seen_at: Time.current)

    get hub_session_preview_path(other_hub, @session_uuid)

    assert_response :redirect
    assert_redirected_to hubs_path
  ensure
    other_hub&.destroy
  end

  test "redirects for nonexistent hub" do
    get hub_session_preview_path(hub_id: 999_999, session_uuid: @session_uuid)

    assert_response :redirect
    assert_redirected_to hubs_path
  end

  # === Public Preview ===

  test "public preview bootstrap allows unauthenticated access" do
    sign_out @user
    @hub.enable_public_preview!(@session_uuid, 8080)

    get hub_session_preview_path(@hub, @session_uuid)

    assert_response :success
    assert_equal "no-store", response.headers["Cache-Control"]
    assert_equal "noindex, nofollow", response.headers["X-Robots-Tag"]
  end

  test "public preview shell omits crypto assets" do
    sign_out @user
    @hub.enable_public_preview!(@session_uuid, 8080)

    get hub_session_preview_shell_path(@hub, @session_uuid)

    assert_response :success
    assert_no_match(/crypto-worker-url/, response.body)
    assert_match 'data-controller="public-preview"', response.body
  end

  test "authenticated shell includes crypto assets" do
    get hub_session_preview_shell_path(@hub, @session_uuid)

    assert_response :success
    assert_match "crypto-worker-url", response.body
    assert_match 'data-controller="preview"', response.body
  end

  test "authenticated owner uses private preview even when public preview is enabled" do
    @hub.enable_public_preview!(@session_uuid, 8080)

    get hub_session_preview_shell_path(@hub, @session_uuid)

    assert_response :success
    assert_match "crypto-worker-url", response.body
    assert_match 'data-controller="preview"', response.body
    assert_no_match 'data-controller="public-preview"', response.body
    assert_match(/allow-same-origin/, response.body)
    assert_match(/data-preview-port-value="8080"/, response.body)
    assert_no_match(/data-public-preview-port-value="8080"/, response.body)
  end

  test "public preview redirects to login when hub is not alive" do
    sign_out @user
    @hub.enable_public_preview!(@session_uuid, 8080)
    @hub.update!(alive: false)

    get hub_session_preview_path(@hub, @session_uuid)

    # Hub offline = public preview not available, falls through to auth
    assert_response :redirect
  end

  test "public preview requires session in public_preview_sessions" do
    sign_out @user
    # Hub alive but no public preview for this session

    get hub_session_preview_path(@hub, @session_uuid)

    # Should redirect to login (not a public preview)
    assert_response :redirect
  end

  test "public preview service worker sets headers" do
    sign_out @user
    @hub.enable_public_preview!(@session_uuid, 8080)

    get hub_session_preview_service_worker_path(@hub, @session_uuid)

    assert_response :success
    assert_equal "no-store", response.headers["Cache-Control"]
    assert_equal "application/javascript", response.headers["Content-Type"]
  end

  test "public preview shell passes the correct forwarded port" do
    sign_out @user
    @hub.enable_public_preview!(@session_uuid, 8080)

    get hub_session_preview_shell_path(@hub, @session_uuid)

    assert_response :success
    assert_match 'data-public-preview-port-value="8080"', response.body
  end

  test "public preview shell does not include allow-same-origin in iframe sandbox" do
    sign_out @user
    @hub.enable_public_preview!(@session_uuid, 8080)

    get hub_session_preview_shell_path(@hub, @session_uuid)

    assert_response :success
    assert_match 'sandbox="allow-scripts allow-forms allow-popups allow-modals"', response.body
    assert_no_match(/allow-same-origin/, response.body)
  end

  test "authenticated preview shell includes allow-same-origin in iframe sandbox" do
    get hub_session_preview_shell_path(@hub, @session_uuid)

    assert_response :success
    assert_match "allow-same-origin", response.body
  end

  test "authenticated bootstrap preserves preview port in shell path" do
    get "#{hub_session_preview_path(@hub, @session_uuid)}?port=46000"

    assert_response :success
    assert_match(/\/preview\/shell\?port=46000/, response.body)
  end

  test "authenticated bootstrap preserves preview path in shell path" do
    get "#{hub_session_preview_path(@hub, @session_uuid)}?port=46000&path=%2Fusers%2Fsign_in"

    assert_response :success
    assert_match(%r{/preview/shell\?(?:path=%2Fusers%2Fsign_in(?:&|&amp;)port=46000|port=46000(?:&|&amp;)path=%2Fusers%2Fsign_in)}, response.body)
  end

  test "authenticated shell passes preview port from params" do
    get "#{hub_session_preview_shell_path(@hub, @session_uuid)}?port=46000"

    assert_response :success
    assert_match(/data-preview-port-value="46000"/, response.body)
  end

  test "authenticated shell passes preview path from params" do
    get "#{hub_session_preview_shell_path(@hub, @session_uuid)}?path=%2Fusers%2Fsign_in"

    assert_response :success
    assert_match(/data-preview-initial-url-value="\/users\/sign_in"/, response.body)
  end
end
