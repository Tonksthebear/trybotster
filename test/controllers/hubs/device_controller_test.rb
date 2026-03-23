# frozen_string_literal: true

require "test_helper"

module Hubs
  class DeviceControllerTest < ActionDispatch::IntegrationTest
    include Devise::Test::IntegrationHelpers

    setup do
      @user = users(:primary_user)
      @hub = hubs(:active_hub)
    end

    test "show displays spawn target browser section" do
      sign_in @user

      get hub_device_path(@hub)

      assert_response :success
      assert_select "[data-controller='spawn-target-browser']"
      assert_select "h2", text: "Admitted Spawn Targets"
    assert_match "Current Hub", response.body
    assert_match "Admit Spawn Target", response.body
    assert_match "spawn-target-path-suggestions", response.body
    assert_match "Start typing an absolute path to browse directories on this device", response.body
  end
end
end
