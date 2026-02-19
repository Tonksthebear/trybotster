# frozen_string_literal: true

require "test_helper"

class Hubs::PairingControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:jason)
    @hub = hubs(:active_hub)
  end

  test "pairing page requires authentication" do
    get hub_pairing_path(@hub)
    assert_redirected_to root_path
  end

  test "pairing page renders for authenticated user" do
    sign_in @user
    get hub_pairing_path(@hub)
    assert_response :success

    assert_match "Secure Pairing", response.body
  end

  test "pairing page includes pairing stimulus controller" do
    sign_in @user
    get hub_pairing_path(@hub)
    assert_response :success

    assert_select "[data-controller='pairing']"
    assert_select "[data-pairing-hub-id-value=?]", @hub.id.to_s
  end

  test "pairing page includes redirect URL to hub show" do
    sign_in @user
    get hub_pairing_path(@hub)
    assert_response :success

    assert_select "[data-pairing-redirect-url-value=?]", hub_path(@hub)
  end

  test "pairing page redirects for non-existent hub" do
    sign_in @user
    get hub_pairing_path("non-existent-hub-id")

    assert_redirected_to hubs_path
    assert_equal "Hub not found", flash[:alert]
  end

  test "pairing page does not allow access to other users hubs" do
    other_user = users(:one)
    other_hub = Hub.create!(
      user: other_user,
      identifier: "other-hub-pairing-test",
      last_seen_at: Time.current
    )

    sign_in @user
    get hub_pairing_path(other_hub)

    assert_redirected_to hubs_path
    assert_equal "Hub not found", flash[:alert]
  ensure
    other_hub&.destroy
  end
end
