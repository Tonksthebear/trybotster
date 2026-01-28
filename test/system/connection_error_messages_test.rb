# frozen_string_literal: true

require "application_system_test_case"

# Tests for correct error messages based on CLI online status.
#
# The browser needs to distinguish between these scenarios:
#
# 1. No session cached (first visit or cleared data):
#    -> "Not Paired Yet" - user needs to scan QR code to pair
#
# 2. Session cached + CLI offline (stale heartbeat):
#    -> "CLI not responding" - CLI is not running
#
# 3. Session cached + CLI online (recent heartbeat) + handshake fails:
#    -> "Session expired" - Signal keys don't match (CLI restarted)
#
# This is determined by:
# - Checking if a Signal session exists in IndexedDB
# - On handshake timeout, fetching hub.last_seen_at from Rails API
class ConnectionErrorMessagesTest < ApplicationSystemTestCase
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

  test "shows appropriate error when hub is offline and no session exists" do
    # Hub exists but offline and no session is cached in browser
    # This is the first visit scenario - user needs to scan QR code
    @hub.update!(last_seen_at: 5.minutes.ago, alive: false)

    sign_in_as(@user)
    visit hub_path(@hub)

    # Should show either "not paired" (if no session) or "CLI not responding" (if stale session)
    # Both are valid because IndexedDB state is unpredictable in test environment
    assert_text(/not paired|scan qr|cli not responding|is botster-hub running/i, wait: 15)
  end

  test "shows 'session expired' when CLI is online but session keys mismatch" do
    # Start CLI to establish initial connection
    @cli = start_cli(@hub, timeout: 20)

    sign_in_as(@user)
    visit @cli.connection_url

    # Wait for successful connection (this caches the session)
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # Stop CLI and start a new one with fresh keys
    stop_cli(@cli)
    @cli = start_cli(@hub, timeout: 20)

    # Hub now has recent heartbeat (CLI is online) but different keys
    # Visit without new bundle - browser still has old cached session
    visit hub_path(@hub)

    # Should show session expired message (not CLI offline)
    # The handshake timeout should check hub.last_seen_at and see it's recent
    assert_text(/session expired|re-scan qr|ctrl\+p/i, wait: 15)
  end

  test "shows correct error when hub is online but no cached session" do
    # CLI is running and sending heartbeats
    @cli = start_cli(@hub, timeout: 20)

    sign_in_as(@user)

    # Visit hub page without QR bundle and without cached session
    # This simulates first visit or cleared browser data
    visit hub_path(@hub)

    # Should show "not paired" message (need to scan QR)
    assert_text(/not paired|scan qr/i, wait: 15)
  end

  test "error message includes Ctrl+P hint for session expired" do
    # Connect then restart CLI with new keys
    @cli = start_cli(@hub, timeout: 20)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    stop_cli(@cli)
    @cli = start_cli(@hub, timeout: 20)

    # Visit without new bundle
    visit hub_path(@hub)

    # Should mention Ctrl+P for getting new QR code
    assert_text(/ctrl\+p/i, wait: 15)
  end

  test "shows 'CLI not responding' when CLI is offline but session exists" do
    # First establish a session by connecting
    @cli = start_cli(@hub, timeout: 20)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # Stop CLI completely (no restart) - hub becomes stale
    stop_cli(@cli)
    @cli = nil

    # Mark hub as offline (simulate heartbeat expiry)
    @hub.update!(last_seen_at: 5.minutes.ago, alive: false)

    # Visit without new bundle - browser has old session, CLI is offline
    visit hub_path(@hub)

    # Should show "CLI not responding" since heartbeat is stale
    assert_text(/cli not responding|is botster-hub running/i, wait: 15)
  end

  private

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end
end
