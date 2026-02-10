# frozen_string_literal: true

require "application_system_test_case"
require_relative "../support/cli_test_helper"

class WebrtcConnectionTest < ApplicationSystemTestCase
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

  test "browser establishes WebRTC connection with CLI" do
    @cli = start_cli(@hub)
    url = @cli.connection_url
    assert url.present?, "CLI should produce a connection URL"

    sign_in_as(@user)
    visit url

    # Browser section should reach "connected" (ActionCable signaling up)
    assert_selector(
      "[data-connection-status-target='browserSection'][data-status='connected']",
      wait: 30
    )

    # Connection section should reach "direct" or "relay" (WebRTC data channel open)
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='direct'], " \
      "[data-connection-status-target='connectionSection'][data-state='relay']",
      wait: 30
    )

    # Hub section should show "online" (CLI heartbeating)
    assert_selector(
      "[data-connection-status-target='hubSection'][data-status='online']",
      wait: 30
    )
  end

  test "connection URL with fragment establishes pairing" do
    @cli = start_cli(@hub)
    url = @cli.connection_url
    assert url.present?, "CLI should produce a connection URL"

    # URL should contain a fragment with the Base32-encoded device key bundle
    uri = URI.parse(url)
    assert uri.fragment.present?, "Connection URL should have a fragment"
    # Base32 uses A-Z and 2-7, the bundle should be a substantial string
    assert_match(/\A[A-Z2-7]+\z/, uri.fragment, "Fragment should be Base32-encoded")
    assert uri.fragment.length > 20, "Bundle fragment should be substantial (got #{uri.fragment.length} chars)"

    sign_in_as(@user)
    visit url

    # Verify connection establishes via the pairing bundle
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='direct'], " \
      "[data-connection-status-target='connectionSection'][data-state='relay']",
      wait: 30
    )
  end

  test "connection survives page refresh" do
    @cli = start_cli(@hub)
    url = @cli.connection_url
    assert url.present?, "CLI should produce a connection URL"

    sign_in_as(@user)
    visit url

    # Wait for full connection
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='direct'], " \
      "[data-connection-status-target='connectionSection'][data-state='relay']",
      wait: 30
    )

    # Refresh the page (Olm session should be cached in IndexedDB)
    visit current_url

    # Connection should re-establish from cached session (no fragment needed)
    assert_selector(
      "[data-connection-status-target='browserSection'][data-status='connected']",
      wait: 30
    )

    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='direct'], " \
      "[data-connection-status-target='connectionSection'][data-state='relay']",
      wait: 30
    )

    assert_selector(
      "[data-connection-status-target='hubSection'][data-status='online']",
      wait: 30
    )
  end

  test "without CLI connection shows appropriate state" do
    sign_in_as(@user)
    visit hub_path(@hub)

    # Hub section should show "offline" since CLI is not running
    assert_selector(
      "[data-connection-status-target='hubSection'][data-status='offline']",
      wait: 10
    )

    # Connection section should show "unpaired" or "disconnected" (no crypto session, no CLI)
    connection_section = find("[data-connection-status-target='connectionSection']", wait: 10)
    state = connection_section["data-state"]
    assert_includes(
      %w[unpaired disconnected connecting],
      state,
      "Without CLI, connection state should be unpaired, disconnected, or connecting (got #{state})"
    )
  end

  test "each CLI restart generates new keys" do
    @cli = start_cli(@hub)
    url1 = @cli.connection_url
    assert url1.present?, "First CLI should produce a connection URL"

    fragment1 = URI.parse(url1).fragment
    assert fragment1.present?, "First URL should have a fragment"

    stop_cli(@cli)
    @cli = nil

    # Start a fresh CLI instance (new temp dir = new keys)
    @cli = start_cli(@hub)
    url2 = @cli.connection_url
    assert url2.present?, "Second CLI should produce a connection URL"

    fragment2 = URI.parse(url2).fragment
    assert fragment2.present?, "Second URL should have a fragment"

    assert_not_equal fragment1, fragment2,
      "Each CLI restart should generate a different key bundle"
  end

  private

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end
end
