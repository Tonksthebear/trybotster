# frozen_string_literal: true

require "test_helper"

class Settings::DevicesControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:jason)
    @device = devices(:cli_device)
    sign_in @user
  end

  # ========== Index ==========

  test "GET /settings/devices renders for authenticated user" do
    get settings_devices_path
    assert_response :success
  end

  test "GET /settings/devices redirects unauthenticated user" do
    sign_out @user
    get settings_devices_path
    assert_response :redirect
  end

  # ========== Show ==========

  test "GET /settings/devices/:id renders for owned device" do
    get settings_device_path(@device)
    assert_response :success
  end

  test "GET /settings/devices/:id returns 404 for another user's device" do
    other_user = users(:one)
    other_device = other_user.devices.create!(
      name: "Other CLI",
      device_type: "cli",
      fingerprint: "ff:ee:dd:cc:bb:aa:99:88"
    )

    get settings_device_path(other_device)
    assert_response :not_found
  ensure
    other_device&.destroy
  end

  test "GET /settings/devices/:id returns 404 for nonexistent device" do
    get settings_device_path(id: 999_999)
    assert_response :not_found
  end
end
