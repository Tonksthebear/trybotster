# frozen_string_literal: true

require "test_helper"

class SettingsTemplatesTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:jason)
    @hub = hubs(:active_hub)
    sign_in @user
  end

  test "settings page renders template catalog" do
    get hub_settings_path(@hub)
    assert_response :success

    # Should render the GitHub plugin template card
    assert_match "GitHub Integration", response.body
    assert_match "plugins-github", response.body
  end

  test "settings page includes template content in data attribute" do
    get hub_settings_path(@hub)
    assert_response :success

    # The preview panel should include the template content for DataChannel install
    assert_match "Github::EventsChannel", response.body
  end

  test "settings page includes template dest path" do
    get hub_settings_path(@hub)
    assert_response :success

    assert_match "plugins/github/init.lua", response.body
  end

  test "settings page renders all template categories" do
    get hub_settings_path(@hub)
    assert_response :success

    assert_match "plugins", response.body
    assert_match "sessions", response.body
    assert_match "initialization", response.body
  end
end
