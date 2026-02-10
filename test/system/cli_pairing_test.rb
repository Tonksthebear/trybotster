# frozen_string_literal: true

require "application_system_test_case"
require "support/cli_test_helper"

# System tests for the CLI pairing link (QR code URL) flow.
#
# The CLI generates a connection URL containing a PreKeyBundle in the URL
# fragment. The browser parses this Base32-encoded bundle to establish an
# E2E encrypted Olm session, then negotiates a WebRTC connection.
#
# Flow:
#   CLI starts -> writes connection_url.txt -> URL has /hubs/:id#<base32_bundle>
#   Browser visits URL -> parses fragment -> creates Olm session via SharedWorker
#   WebRTC connection established -> status indicators turn green
#
class CliPairingTest < ApplicationSystemTestCase
  include CliTestHelper
  include WaitHelper

  driven_by :selenium, using: :headless_chrome, screen_size: [1280, 900]

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
  # - Decoded fragment is exactly 161 bytes (v6 DeviceKeyBundle)
  # - First byte is version 0x06
  test "CLI generates valid connection URL with correct path and bundle" do
    @cli = start_cli(@hub)

    url = @cli.connection_url
    assert url.present?, "CLI should write connection_url.txt"

    # Parse the URL components
    uri = URI.parse("http://localhost#{url}")
    fragment = uri.fragment

    # Path should be /hubs/:id (Rails integer ID)
    assert_match %r{\A/hubs/\d+\z}, uri.path,
      "URL path should be /hubs/:id, got: #{uri.path}"

    # Fragment should be present and reasonably long (Base32 of 161 bytes ~ 258 chars)
    assert fragment.present?, "URL should have a fragment (Base32-encoded bundle)"
    assert fragment.length >= 250, "Fragment should be ~258+ Base32 chars, got #{fragment.length}"

    # Decode and validate the binary bundle
    bytes = base32_decode(fragment)
    assert_equal 161, bytes.length, "Decoded bundle should be exactly 161 bytes, got #{bytes.length}"
    assert_equal 0x06, bytes[0], "Bundle version byte should be 0x06 (v6), got 0x#{bytes[0].to_s(16)}"
  end

  # -- Test 2: Pairing URL establishes encrypted session ---------------------
  #
  # The full happy path: CLI running, browser visits connection URL with
  # the PreKeyBundle fragment, connection status progresses to connected.
  test "pairing URL establishes encrypted session and WebRTC connection" do
    @cli = start_cli(@hub)

    url = @cli.connection_url
    assert url.present?, "CLI should provide connection URL"

    sign_in_as(@user)
    visit "#{page.server_url}#{url}"

    # All three status sections should reach their connected states
    assert_selector(
      "[data-connection-status-target='browserSection'][data-status='connected']",
      wait: 25
    )
    assert_selector(
      "[data-connection-status-target='hubSection'][data-status='online']",
      wait: 25
    )
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='direct'], " \
      "[data-connection-status-target='connectionSection'][data-state='relay']",
      wait: 30
    )
  end

  # -- Test 3: Invalid/corrupted bundle shows error state --------------------
  #
  # Visiting a hub URL with garbage in the fragment should not crash.
  # The connection section should show "unpaired" since the browser cannot
  # parse a valid bundle from the corrupted data.
  test "corrupted bundle in fragment shows unpaired state" do
    sign_in_as(@user)
    visit "#{page.server_url}/hubs/#{@hub.id}#INVALIDGARBAGE1234567890ABCDEF"

    # Connection section should show "unpaired" (bundle parse failure -> no Olm session)
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='unpaired']",
      wait: 20
    )
  end

  # -- Test 4: Stale session requires new QR scan ---------------------------
  #
  # Scenario: Browser connected to CLI #1, CLI #1 stops, CLI #2 starts with
  # new keys. The browser's cached Olm session (from CLI #1) is now stale
  # and should not auto-reconnect to CLI #2. The connection should show
  # "expired" or "unpaired", forcing the user to scan a new QR code.
  test "stale session does not reconnect after CLI restart with new keys" do
    # Phase 1: Establish connection with first CLI
    @cli = start_cli(@hub)
    url = @cli.connection_url
    assert url.present?

    sign_in_as(@user)
    visit "#{page.server_url}#{url}"

    # Wait for connection to fully establish
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='direct'], " \
      "[data-connection-status-target='connectionSection'][data-state='relay']",
      wait: 30
    )

    # Phase 2: Stop CLI and start a new one (new keypair)
    stop_cli(@cli)
    @cli = nil

    # Wait for browser to detect disconnection
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='disconnected'], " \
      "[data-connection-status-target='connectionSection'][data-state='expired'], " \
      "[data-connection-status-target='connectionSection'][data-state='unpaired']",
      wait: 15
    )

    @cli = start_cli(@hub)
    new_url = @cli.connection_url
    assert new_url.present?

    # Phase 3: Visit the hub page WITHOUT the new bundle fragment.
    # The browser still has the stale Olm session from CLI #1.
    visit "#{page.server_url}/hubs/#{@hub.id}"

    # The connection should NOT reach "direct" or "relay" — the old session
    # is incompatible with the new CLI's keys. It should show expired or unpaired.
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='expired'], " \
      "[data-connection-status-target='connectionSection'][data-state='unpaired']",
      wait: 25
    )
  end

  # -- Test 5: Hub page without fragment shows scan prompt -------------------
  #
  # Visiting the hub URL with no fragment at all (no bundle) means the
  # browser has no way to create an Olm session. The connection section
  # should show "unpaired" with the QR code icon (Scan Code indicator).
  test "hub page without fragment shows unpaired scan prompt" do
    sign_in_as(@user)
    visit "#{page.server_url}/hubs/#{@hub.id}"

    # Connection section should show "unpaired"
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='unpaired']",
      wait: 20
    )
  end

  private

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end

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
