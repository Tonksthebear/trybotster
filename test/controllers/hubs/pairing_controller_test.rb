# frozen_string_literal: true

require "test_helper"

class Hubs::PairingControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:primary_user)
    @hub = hubs(:active_hub)
  end

  test "pairing page is public so the URL fragment survives the click" do
    # The CLI prints `https://<host>/hubs/<id>/pairing#<bundle>`. Browsers do
    # not send fragments to the server, so a Devise OAuth bounce strips the
    # bundle from the redirect chain — leaving the user on the home page with
    # no way to recover the pairing material. The bundle itself is the
    # credential; the SPA validates it client-side.
    get hub_pairing_path(@hub)
    assert_response :success
    assert_select "#app"
  end

  test "pairing page serves SPA shell when signed in" do
    sign_in @user
    get hub_pairing_path(@hub)
    assert_response :success
    assert_select "#app"
  end

  test "pairing page renders SPA shell even for an unknown hub id" do
    # The SPA reports "hub not found" itself once it tries to load entities.
    # The controller no longer redirects so a stale URL on someone's clipboard
    # does not bounce them to a Devise login.
    get hub_pairing_path("non-existent-hub-id")
    assert_response :success
    assert_select "#app"
  end

  test "pairing page is reachable across users — bundle is the credential" do
    other_user = users(:one)
    other_hub = Hub.create!(
      user: other_user,
      identifier: "other-hub-pairing-test",
      last_seen_at: Time.current
    )

    sign_in @user
    get hub_pairing_path(other_hub)

    assert_response :success
    assert_select "#app"
  ensure
    other_hub&.destroy
  end
end
