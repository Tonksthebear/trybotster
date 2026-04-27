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

  test "settings JSON includes markdown session template files" do
    get hub_settings_path(@hub), as: :json
    assert_response :success

    json = JSON.parse(response.body)
    agent_dests = json.dig("templates", "agents").map { |template| template["dest"] }
    assert_includes agent_dests, "agents/claude/notes.md"
    quick_setup_dests = json["agentTemplates"].map { |template| template["dest"] }
    assert_includes quick_setup_dests, "agents/claude/initialization"
    refute_includes quick_setup_dests, "agents/claude/notes.md"
  end

  test "settings JSON includes nested multi-file plugin templates" do
    get hub_settings_path(@hub), as: :json
    assert_response :success

    json = JSON.parse(response.body)
    plugin_dests = json.dig("templates", "plugins").map { |template| template["dest"] }
    assert_includes plugin_dests, "plugins/demo-surface/init.lua"
    assert_includes plugin_dests, "plugins/demo-surface/web_layout.lua"
    assert_includes plugin_dests, "plugins/demo-surface/tui/status.lua"
  end

  test "settings update returns JSON for JSON requests" do
    patch hub_settings_path(@hub), params: { hub: { name: "Updated Hub" } }, as: :json
    assert_response :success

    json = JSON.parse(response.body)
    assert_equal @hub.id, json["id"]
    assert_equal "Updated Hub", json["name"]
    assert_equal @hub.identifier, json["identifier"]
  end

  test "settings destroy returns no content for JSON requests" do
    assert_difference("Hub.count", -1) do
      delete hub_settings_path(@hub), as: :json
    end

    assert_response :no_content
  end
end
