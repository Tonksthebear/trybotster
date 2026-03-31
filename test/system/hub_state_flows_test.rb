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
    prime_server!
    @cli = start_cli(@hub)
    associate_hub_device!

    sign_in_and_connect

    open_new_session_chooser

    assert_selector "[data-new-session-chooser-target='targetSection']", wait: 10
    assert_no_selector "[data-new-session-chooser-target='targetSection'].hidden", visible: :all

    target_select = find("[data-new-session-chooser-target='targetSelect']", wait: 10)
    target_options = target_select.all("option").map(&:text)
    assert target_options.any? { |text| text.include?(File.basename(@cli.temp_dir)) },
      "Expected chooser options to include the admitted spawn target label"

    assert_selector "[data-new-session-chooser-target='agentButton'][disabled]", wait: 5
    assert_selector "[data-new-session-chooser-target='accessoryButton'][disabled]", wait: 5

    selectable_option = target_select.all("option").find { |option| option.value.present? }
    assert selectable_option, "Expected at least one admitted spawn target option"

    target_select.select(selectable_option.text)

    assert_no_selector "[data-new-session-chooser-target='agentButton'][disabled]", wait: 10
    assert_no_selector "[data-new-session-chooser-target='accessoryButton'][disabled]", wait: 10
    assert_text "Spawn target selected. Now choose whether to start an agent or an accessory."
  end

  test "new agent modal preserves the selected target from the chooser" do
    prime_server!
    @cli = start_cli(@hub)
    associate_hub_device!

    sign_in_and_connect

    open_new_session_chooser

    # Wait for async spawn target load — options may re-render after initial mount.
    selected_value = nil
    selected_text = nil
    assert wait_until?(timeout: 15, poll: 0.3) {
      target_select = find("[data-new-session-chooser-target='targetSelect']", wait: 2)
      option = target_select.all("option").find { |o| o.value.present? rescue false }
      if option
        selected_value = option.value
        selected_text = option.text
        true
      end
    }, "Expected at least one admitted spawn target option"

    find("[data-new-session-chooser-target='targetSelect']", wait: 5).select(selected_text)
    find("[data-new-session-chooser-target='agentButton']", wait: 10).click

    assert_selector "dialog#new-agent-modal[open]", wait: 10
    assert_no_selector "[data-new-agent-form-target='targetSection'].hidden", visible: :all, wait: 10
    assert_no_selector "[data-new-agent-form-target='worktreeOptions'].hidden", visible: :all, wait: 10

    modal_target_select = find("[data-new-agent-form-target='targetSelect']", wait: 10)
    assert_equal selected_value, modal_target_select.value
  end

  test "quick setup hides the no-agent-config banner after installing the template" do
    prime_server!
    @cli = start_cli(@hub)
    associate_hub_device!

    sign_in_and_connect

    assert_selector "[data-hub-setup-banner-target='banner']:not(.hidden)", wait: 15

    find("[data-action='hub-setup-banner#quickSetup']:not([disabled])", wait: 10).click

    assert_no_selector "[data-hub-setup-banner-target='banner']:not(.hidden)", wait: 15
  end

  private

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end

  def prime_server!
    sign_in_as(@user)
  end

  def associate_hub_device!
    # No-op after Device→Hub collapse: Hub IS the device now.
  end

  def sign_in_and_connect
    url = @cli.connection_url
    assert url.present?, "CLI should produce a connection URL"

    sign_in_as(@user)
    visit url

    assert_selector "[data-pairing-target='ready']", wait: 15
    find("[data-action='pairing#pair']").click

    assert_selector "[data-connection-status-target='connectionSection']", wait: 15
    assert_webrtc_connected
  end

  def assert_webrtc_connected(wait: 30)
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='direct'], " \
      "[data-connection-status-target='connectionSection'][data-state='relay']",
      wait: wait,
    )
  end

  def open_new_session_chooser
    first("[commandfor='new-session-chooser-modal']:not([disabled])", wait: 10).click
    assert_selector "dialog#new-session-chooser-modal[open]", wait: 10
  end
end
