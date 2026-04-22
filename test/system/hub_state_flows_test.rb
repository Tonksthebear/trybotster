# frozen_string_literal: true

require "application_system_test_case"
require_relative "../support/cli_test_helper"

class HubStateFlowsTest < ApplicationSystemTestCase
  include CliTestHelper

  driven_by :selenium, using: :headless_chrome, screen_size: [ 1280, 900 ]

  setup do
    @user = users(:one)
    @hub = create_test_hub(user: @user)
  end

  teardown do
    stop_cli(@cli) if @cli
    @hub&.destroy
  end

  test "new session chooser loads admitted spawn targets and unlocks choices after selection" do
    @cli = start_cli(@hub)

    sign_in_and_connect

    open_new_session_chooser

    assert_selector "[data-new-session-chooser-target='targetSection']", wait: 10
    assert_no_selector "[data-new-session-chooser-target='targetSection'].hidden", visible: :all

    find("[data-new-session-chooser-target='targetSelect']", wait: 10)
    selected_option = wait_for_spawn_target_option
    assert selected_option[:text].include?(File.basename(@cli.temp_dir)),
      "Expected chooser options to include the admitted spawn target label"

    assert_selector "[data-new-session-chooser-target='agentButton'][disabled]", wait: 5
    assert_selector "[data-new-session-chooser-target='accessoryButton'][disabled]", wait: 5

    find("[data-new-session-chooser-target='targetSelect']", wait: 5).select(selected_option[:text])

    assert_no_selector "[data-new-session-chooser-target='agentButton'][disabled]", wait: 10
    assert_no_selector "[data-new-session-chooser-target='accessoryButton'][disabled]", wait: 10
    assert_text "Spawn target selected. Now choose whether to start an agent or an accessory."
  end

  test "new agent modal preserves the selected target from the chooser" do
    @cli = start_cli(@hub)

    sign_in_and_connect

    open_new_session_chooser

    selected_option = wait_for_spawn_target_option
    find("[data-new-session-chooser-target='targetSelect']", wait: 5).select(selected_option[:text])
    find("[data-testid='choose-agent']", wait: 10).click

    assert_text "New Agent", wait: 10

    modal_target_select = find("[data-testid='new-agent-target-select']", wait: 10)
    assert_equal selected_option[:value], modal_target_select.value
  end

  test "device config tree shows add-session controls when hub config is available" do
    @cli = start_cli(@hub)

    sign_in_and_connect

    click_link "Hub Settings"
    click_button "Device"
    wait_for_settings_ready("device", state: "tree")
    assert_selector "[data-hub-settings-target='treePanel'][data-view='tree']"
    assert_no_selector "[data-hub-setup-banner-target='banner']", wait: 2
    assert_button "+ Add Agent"
    assert_button "+ Add Accessory"
  end

  private

  def open_new_session_chooser
    wait_for_surface_ready("workspace_panel")
    find("[data-testid='new-session-button']:not([disabled])", match: :first).click
    assert_text "New Session", wait: 10
  end

  def wait_for_spawn_target_option(timeout: 15)
    option_value = nil
    option_text = nil

    assert wait_until?(timeout: timeout, poll: 0.3) {
      target_select = find("[data-new-session-chooser-target='targetSelect']", wait: 2)
      option = target_select.all("option").find { |candidate| candidate.value.present? rescue false }
      if option
        option_value = option.value
        option_text = option.text
        true
      end
    }, "Expected at least one admitted spawn target option"

    { value: option_value, text: option_text }
  end
end
