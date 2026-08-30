# frozen_string_literal: true

require "application_system_test_case"
require "support/cli_test_helper"

# System tests for the CLI pairing link (QR code URL) flow.
#
# The CLI generates a connection URL with a pairing bundle in the fragment.
# The browser keeps its signing key and authenticates inside the DataChannel.
#
# Flow:
#   CLI starts -> writes connection_url.txt -> URL has /hubs/:id#<base32_bundle>
#   Browser visits URL -> stores the pinned hub key and its browser signing key
#   WebRTC connection established -> status indicators turn green
#
class CliPairingTest < ApplicationSystemTestCase
  include CliTestHelper
  include WaitHelper

  driven_by :selenium, using: :headless_chrome, screen_size: [ 1280, 900 ]

  setup do
    @user = users(:one)
    @hub = create_test_hub(user: @user)
  end

  teardown do
    stop_cli(@cli) if @cli
    @hub&.destroy
  end

  # -- Test 1: CLI generates a valid connection URL --------------------------
  #
  # Verifies the CLI writes a connection_url.txt with the expected structure:
  # - Path matches /hubs/:id
  # - Fragment is present and Base32-encoded
  # - Decoded fragment contains route, hub key, secret, and expiry
  # - First byte is version 0x07
  test "CLI generates valid connection URL with correct path and bundle" do
    @cli = start_cli(@hub)

    url = @cli.connection_url
    assert url.present?, "CLI should write connection_url.txt"

    # Parse the URL components (CLI writes full URL with scheme + host)
    uri = URI.parse(url)
    fragment = uri.fragment

    # Path should be /hubs/:id/pairing (Rails integer ID)
    assert_match %r{\A/hubs/\d+/pairing\z}, uri.path,
      "URL path should be /hubs/:id/pairing, got: #{uri.path}"

    # The fragment carries all pairing data.
    assert fragment.present?, "URL should have a fragment (Base32-encoded bundle)"
    assert fragment.length >= 120, "Pairing fragment is too short: #{fragment.length}"

    # Decode and validate the binary bundle
    bytes = base32_decode(fragment)
    route_length = bytes[9, 2].pack("C*").unpack1("n")
    assert_equal 11 + route_length + 64, bytes.length
    assert_equal 0x07, bytes[0]
    assert_equal "/hubs/#{@cli.hub.id}", bytes[11, route_length].pack("C*")
  end

  # -- Test 2: Pairing URL authenticates a WebRTC connection -----------------
  #
  # The full happy path: CLI running, browser visits connection URL with
  # the PreKeyBundle fragment, connection status progresses to connected.
  test "pairing URL authenticates a WebRTC connection" do
    @cli = start_cli(@hub)

    url = @cli.connection_url
    assert url.present?, "CLI should provide connection URL"

    sign_in_as(@user)
    visit url

    complete_pairing_for(@hub, pairing_url: url)

    # All three status sections should reach their connected states
    assert_sidebar_connection_status(browser: "connected", wait: 25)
    assert_sidebar_connection_status(hub: "online", wait: 25)
    assert_sidebar_webrtc_connected(wait: 30)
  end

  # -- Test 3: Invalid/corrupted bundle shows error state --------------------
  #
  # Visiting a hub URL with garbage in the fragment should not crash.
  # The connection section should show "unpaired" since the browser cannot
  # parse a valid bundle from the corrupted data.
  test "corrupted bundle in fragment shows unpaired state" do
    sign_in_as(@user)
    visit "#{page.server_url}/hubs/#{@hub.id}#INVALIDGARBAGE1234567890ABCDEF"

    # Connection section shows "unpaired" after bundle parsing fails.
    # The state transitions: connecting -> (ActionCable subscribes) -> unpaired.
    assert_sidebar_connection_status(connection: "unpaired", wait: 45)
  end

  # -- Test 4: Stale session requires new QR scan ---------------------------
  #
  # Scenario: Browser connected to CLI #1, CLI #1 stops, CLI #2 starts with
  # a changed hub key. The browser must reject that changed trust anchor
  # and should not auto-reconnect to CLI #2. The connection should surface
  # an expired/stale-session state instead of silently reusing bad keys.
  test "stale session does not reconnect after CLI restart with new keys" do
    # Phase 1: Establish connection with first CLI
    @cli = start_cli(@hub)
    url = @cli.connection_url
    assert url.present?

    sign_in_as(@user)
    visit url

    complete_pairing_for(@hub, pairing_url: url)

    # Wait for connection to fully establish
    assert_sidebar_webrtc_connected(wait: 30)

    # Phase 2: Stop CLI and start a new one (new keypair)
    stop_cli(@cli)
    @cli = nil

    # Wait for browser to detect disconnection
    assert_selector(
      "#{SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='disconnected'], " \
      "#{SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='expired'], " \
      "#{SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='unpaired']",
      wait: 15
    )

    @cli = start_cli(@hub)
    new_url = @cli.connection_url
    assert new_url.present?

    # Phase 3: Visit the hub page WITHOUT the new bundle fragment.
    # The browser still pins the first hub key, so the SPA
    # should prompt for a fresh pairing instead of reconnecting automatically.
    visit hub_path(@hub)

    assert_sidebar_connection_status(browser: "connected", wait: 30)
    assert_sidebar_connection_status(hub: "online", wait: 30)
    assert_selector(
      "#{SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='expired'], " \
      "#{SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='pairing_needed']",
      wait: 30
    )
    assert(
      page.has_button?("Retry connection", wait: 5) || page.has_button?("Start pairing", wait: 5),
      "Expected stale session recovery UI"
    )
  end

  # -- Test 5: Hub page without fragment shows scan prompt -------------------
  #
  # Visiting the hub URL with no fragment at all (no bundle) means the
  # browser has no stored browser credential. The connection section
  # should show "unpaired" with the QR code icon (Scan Code indicator).
  test "hub page without fragment shows unpaired scan prompt" do
    sign_in_as(@user)
    visit "#{page.server_url}/hubs/#{@hub.id}"

    # Connection section shows "unpaired" without a browser credential.
    # The state transitions: connecting -> (ActionCable subscribes) -> unpaired.
    assert_sidebar_connection_status(connection: "unpaired", wait: 45)
  end

  private

  # Decode Base32 (RFC 4648) to byte array, matching the browser's base32Decode.
  def base32_decode(input)
    alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
    input = input.upcase.gsub(/=+$/, "").gsub(/[^A-Z2-7]/, "")

    bits = input.chars.map { |c|
      idx = alphabet.index(c)
      raise "Invalid Base32 character: #{c}" unless idx
      idx.to_s(2).rjust(5, "0")
    }.join

    byte_count = bits.length / 8
    byte_count.times.map { |i| bits[i * 8, 8].to_i(2) }
  end
end
