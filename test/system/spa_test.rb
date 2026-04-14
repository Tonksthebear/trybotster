# frozen_string_literal: true

require "application_system_test_case"

class SpaTest < ApplicationSystemTestCase
  driven_by :selenium, using: :headless_chrome, screen_size: [1400, 900]

  setup do
    @user = users(:primary_user)
    @hub = hubs(:active_hub)
  end

  # --- Public pages ---

  test "root URL loads and React renders" do
    visit "/"
    assert_selector "#app", wait: 10
    assert_text "botster", wait: 10
  end

  # --- Authenticated pages ---

  test "hub dashboard loads with hub list" do
    login_as @user
    visit "/hubs"
    assert_selector "#app", wait: 10
    # The HubDashboard fetches hubs via JSON API
    assert_text "Hubs", wait: 10
  end

  test "hub show page loads" do
    login_as @user
    visit "/hubs/#{@hub.id}"
    assert_selector "#app", wait: 10
    # CSS uppercase makes "Hub Information" render as "HUB INFORMATION"
    assert_text "HUB INFORMATION", wait: 10
  end

  test "settings page loads with header and tabs" do
    login_as @user
    visit "/hubs/#{@hub.id}/settings"
    assert_selector "#app", wait: 10
    # Settings title or tab
    assert_text "Config", wait: 10
  end

  test "pairing page loads" do
    login_as @user
    visit "/hubs/#{@hub.id}/pairing"
    assert_selector "#app", wait: 10
    # PairingPage renders pairing content
    assert_text "Secure Pairing", wait: 10
  end

  test "session page loads" do
    login_as @user
    visit "/hubs/#{@hub.id}/sessions/test-session-uuid"
    assert_selector "#app", wait: 10
    # TerminalView renders — check for terminal container
    assert_selector ".terminal-container", wait: 10
  end

  # --- Auth redirects ---

  test "unauthenticated user accessing hubs gets redirected" do
    visit "/hubs"
    # Devise should redirect to login
    assert_no_current_path "/hubs"
  end

  # --- Navigation ---

  test "navigating from hub show to settings preserves SPA" do
    login_as @user
    visit "/hubs/#{@hub.id}"
    assert_text "HUB INFORMATION", wait: 10

    # Click Settings link
    click_link "Settings"
    assert_text "Config", wait: 10
    assert_current_path "/hubs/#{@hub.id}/settings"
  end

  # --- User account settings (ERB, not SPA) ---

  test "user settings page loads for authenticated users" do
    login_as @user
    visit "/settings"
    assert_text "Settings", wait: 10
  end

  # --- Hub settings fetches data from API ---

  test "hub settings shows config tab content" do
    login_as @user
    visit "/hubs/#{@hub.id}/settings"
    assert_selector "#app", wait: 10
    assert_text "Config", wait: 10
    # Settings page should have loaded (not showing "Loading settings...")
    assert_no_text "Loading settings...", wait: 10
  end

  # --- Logout from docs/application layout ---

  test "docs page loads with logout button" do
    login_as @user
    visit "/docs"
    assert_text "Docs", wait: 10
    # Logout button exists as a form (button_to generates a form)
    assert_selector "form[action='/logout'] input[type='submit'], form[action='/logout'] button", wait: 10
  end

  # --- No JS errors ---

  test "hub show page has no JS console errors" do
    login_as @user
    visit "/hubs/#{@hub.id}"
    assert_selector "#app", wait: 10

    errors = page.driver.browser.logs.get(:browser).select { |log|
      log.level == "SEVERE" && !log.message.include?("favicon")
    }
    assert_empty errors, "Expected no JS errors, got: #{errors.map(&:message).join(', ')}"
  end

  private

  def login_as(user)
    Warden.test_mode!
    Warden.test_reset!
    super(user, scope: :user)
  end
end
