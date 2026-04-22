# frozen_string_literal: true

require "test_helper"

# Phase 4a: the /hubs/:hub_id/... routing collapsed from three enumerated
# Rails routes (sessions/settings/pairing) to a single wildcard that lets
# React Router decide which hub-authored surface to mount. These tests lock
# the collapse: arbitrary plugin-authored paths must dispatch to spa#hub
# without a 404, and the legacy paths must still resolve through the same
# catch-all.
class SpaControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:primary_user)
    @active_hub = hubs(:active_hub)
  end

  test "wildcard hub route dispatches to spa#hub for an arbitrary plugin path" do
    sign_in @user
    get "/hubs/#{@active_hub.id}/plugins/hello"
    assert_response :success
    assert_select "#app"
  end

  test "wildcard hub route handles deeper plugin paths" do
    sign_in @user
    get "/hubs/#{@active_hub.id}/plugins/custom-thing/nested/segment"
    assert_response :success
    assert_select "#app"
  end

  test "wildcard hub route handles Phase 4b sub-route URLs with params" do
    # `surfaces.register("kanban", { routes = { { path = "/board/:id" } } })`
    # produces `/hubs/:id/kanban/board/42` as the canonical URL. The Rails
    # catch-all must hand the whole thing to SPA#hub regardless of depth so
    # React Router's DynamicSurfaceRoute can prefix-match `base_path = "/kanban"`.
    sign_in @user
    get "/hubs/#{@active_hub.id}/kanban/board/42"
    assert_response :success
    assert_select "#app"
  end

  test "wildcard hub route tolerates 4+ segment Phase 4b deep links" do
    # A plugin could register `/team/:team_id/board/:board_id/card/:card_id`
    # — four segments after the base. The catch-all must still route
    # regardless of depth.
    sign_in @user
    get "/hubs/#{@active_hub.id}/boards/team/42/board/7/card/99"
    assert_response :success
    assert_select "#app"
  end

  test "wildcard hub route still accepts the legacy sessions path" do
    sign_in @user
    get "/hubs/#{@active_hub.id}/sessions/some-session-uuid"
    assert_response :success
    assert_select "#app"
  end

  test "wildcard hub route still accepts the legacy settings path" do
    sign_in @user
    get "/hubs/#{@active_hub.id}/settings"
    assert_response :success
    assert_select "#app"
  end

  test "wildcard hub route requires authentication" do
    get "/hubs/#{@active_hub.id}/plugins/hello"
    # spa#hub calls authenticate_user! which redirects unauthenticated users
    # to the Devise sign-in. Devise routes to the GitHub authorization flow
    # in this project; either redirect or 401 is acceptable — just assert
    # that unauthenticated requests do NOT render the SPA shell.
    assert_not_equal 200, response.status, "expected auth gate, got #{response.status}"
  end
end
