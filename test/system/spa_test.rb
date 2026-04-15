# frozen_string_literal: true

require "application_system_test_case"
require_relative "../support/cli_test_helper"

class SpaTest < ApplicationSystemTestCase
  include CliTestHelper

  driven_by :selenium, using: :headless_chrome, screen_size: [1400, 900]

  setup do
    @user = users(:one)
    @hub = create_test_hub(user: @user)
  end

  teardown do
    stop_cli(@cli) if @cli
    @hub&.destroy
  end

  # --- Pairing + Connection ---

  test "full pairing flow: visit connection URL, pair, verify connected" do
    prime_server!
    @cli = start_cli(@hub)

    url = @cli.connection_url
    assert url.present?, "CLI should produce a connection URL"

    sign_in_as(@user)
    visit url

    # Pairing page shows ready state with bundle parsed from URL
    assert_selector "[data-testid='pairing-ready']", wait: 15

    # Click Complete Pairing
    click_button "Complete Pairing"

    # Success state shows
    assert_selector "[data-testid='pairing-success']", wait: 15
    assert_text "Paired successfully"
  end

  test "hub page shows connected status after pairing" do
    sign_in_and_connect

    # Connection status should show direct or relay (not connecting)
    assert_webrtc_connected
  end

  # --- Hub page ---

  test "hub page renders sidebar with workspace list" do
    sign_in_and_connect

    # Sidebar shows Workspaces heading
    assert_text "Workspaces", wait: 10

    # Workspace list shows (empty state or sessions)
    assert_text("No sessions running", wait: 10)
  end

  test "hub page renders hub information card" do
    sign_in_and_connect

    assert_text "HUB INFORMATION", wait: 10
    assert_text "Settings"
    assert_text "Share"
  end

  # --- Session creation flow ---

  test "new session chooser opens and loads spawn targets" do
    sign_in_and_connect

    all("button", text: "New session", wait: 10).last.click

    # Chooser dialog opens
    assert_text "New Session", wait: 10
    assert_text "Spawn target", wait: 10

    # Spawn target select populates
    select_el = find("[data-testid='spawn-target-select']", wait: 10)
    assert wait_until?(timeout: 15, poll: 0.3) {
      select_el.all("option").any? { |o| o.value.present? rescue false }
    }, "Expected at least one spawn target option"
  end

  test "selecting spawn target enables agent and accessory buttons" do
    sign_in_and_connect

    all("button", text: "New session", wait: 10).last.click

    # Wait for spawn targets
    select_el = find("[data-testid='spawn-target-select']", wait: 10)
    option_text = nil
    assert wait_until?(timeout: 15, poll: 0.3) {
      opt = select_el.all("option").find { |o| o.value.present? rescue false }
      option_text = opt&.text
      opt
    }, "Expected a spawn target option"

    # Buttons should be disabled before selection
    assert_selector "[data-testid='choose-agent'][disabled]", wait: 5
    assert_selector "[data-testid='choose-accessory'][disabled]", wait: 5

    # Select the spawn target
    select_el.select(option_text)

    # Buttons should be enabled
    assert_no_selector "[data-testid='choose-agent'][disabled]", wait: 10
    assert_no_selector "[data-testid='choose-accessory'][disabled]", wait: 10
  end

  # --- Session spawning ---

  test "full agent creation flow: chooser -> agent form -> submit without JS errors" do
    sign_in_and_connect

    # Open new session chooser
    all("button", text: "New session", wait: 10).last.click

    # Select spawn target
    select_el = find("[data-testid='spawn-target-select']", wait: 10)
    option_text = nil
    assert wait_until?(timeout: 15, poll: 0.3) {
      opt = select_el.all("option").find { |o| o.value.present? rescue false }
      option_text = opt&.text
      opt
    }, "Expected a spawn target option"
    select_el.select(option_text)

    # Click Agent — chooser closes, agent form opens
    find("[data-testid='choose-agent']", wait: 10).click
    assert_text "New Agent", wait: 10

    # Worktree options visible with spawn target pre-selected
    assert_text "Main branch", wait: 10

    # Select main branch
    click_button "Main branch"

    # Step 2 shows with Create button
    assert_selector "button", text: "Create", wait: 10

    # Submit — dialog should close
    click_button "Create"
    assert_no_text "New Agent", wait: 10

    # No JS errors from the entire flow
    errors = page.driver.browser.logs.get(:browser).select { |log|
      log.level == "SEVERE" && !log.message.include?("favicon")
    }
    assert_empty errors, "Expected no JS errors during agent creation, got: #{errors.map(&:message).join(', ')}"
  end

  # --- Navigation ---

  test "sidebar navigation to settings and back preserves connection" do
    sign_in_and_connect

    # Click Hub Settings in sidebar
    click_link "Hub Settings"
    assert_text "Config", wait: 10
    assert_current_path "/hubs/#{@hub.id}/settings"

    # Navigate back to hub
    visit "/hubs/#{@hub.id}"
    assert_text "HUB INFORMATION", wait: 10

    # Connection should still be alive
    assert_webrtc_connected
  end

  test "sidebar navigation is SPA — no full page reload" do
    sign_in_and_connect

    # Set a JS marker on the window — survives SPA navigation, dies on full reload
    page.execute_script("window.__spaMarker = true")

    # Click Hub Settings in sidebar
    click_link "Hub Settings"
    assert_text "Config", wait: 10

    # Marker should survive (SPA nav) — if it's gone, the page fully reloaded
    marker = page.evaluate_script("window.__spaMarker")
    assert_equal true, marker, "Expected SPA navigation but page fully reloaded (window.__spaMarker lost)"

    # Connection should also survive
    assert_webrtc_connected
  end

  # --- No JS errors ---

  test "no severe JS console errors on hub page" do
    sign_in_and_connect

    errors = page.driver.browser.logs.get(:browser).select { |log|
      log.level == "SEVERE" && !log.message.include?("favicon")
    }
    assert_empty errors, "Expected no JS errors, got: #{errors.map(&:message).join(', ')}"
  end

  private

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end

  def prime_server!
    sign_in_as(@user)
  end

  def sign_in_and_connect
    prime_server!
    @cli = start_cli(@hub)

    url = @cli.connection_url
    assert url.present?, "CLI should produce a connection URL"

    sign_in_as(@user)
    visit url

    assert_selector "[data-testid='pairing-ready']", wait: 15
    click_button "Complete Pairing"
    assert_selector "[data-testid='pairing-success']", wait: 15

    visit "/hubs/#{@hub.id}"
    assert_webrtc_connected
  end

  def assert_webrtc_connected(wait: 30)
    # Connection status shows Direct or Relay (text rendered by ConnectionStatus component)
    assert_text(/Direct|Relay/, wait: wait)
  end
end
