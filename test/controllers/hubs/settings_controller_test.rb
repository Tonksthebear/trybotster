# frozen_string_literal: true

require "test_helper"

class Hubs::SettingsControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:primary_user)
    @hub = hubs(:active_hub)
    sign_in @user
  end

  test "settings page serves SPA shell" do
    get hub_settings_path(@hub)
    assert_response :success
    assert_select "#app"
  end

  test "settings JSON returns config metadata" do
    get hub_settings_path(@hub), as: :json
    assert_response :success

    json = JSON.parse(response.body)
    assert json.key?("configMetadata")
    assert json.key?("templates")
    assert json.key?("agentTemplates")
    assert json.key?("hubName")
    assert json.key?("hubIdentifier")
  end

  test "settings JSON includes template catalog" do
    get hub_settings_path(@hub), as: :json
    assert_response :success

    json = JSON.parse(response.body)
    templates = json["templates"]
    assert templates.is_a?(Hash), "Expected templates to be a hash"
  end
end
