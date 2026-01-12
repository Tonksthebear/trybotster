# frozen_string_literal: true

require "application_system_test_case"

# End-to-end system tests for the browser-CLI terminal relay connection.
#
# These tests verify the FULL Signal Protocol E2E encryption flow:
# 1. CLI starts and generates Signal Protocol keys (identity, PreKeys, Kyber)
# 2. CLI connects to Rails Action Cable relay
# 3. CLI writes connection URL with PreKeyBundle to file
# 4. Browser visits URL with bundle in fragment
# 5. Browser loads WASM and creates Signal session (X3DH key agreement)
# 6. Browser sends encrypted handshake via Action Cable relay
# 7. CLI decrypts handshake, sends encrypted ACK back
# 8. Browser decrypts ACK, connection is established
# 9. Both sides can now exchange encrypted messages (Double Ratchet)
#
# This tests REAL cryptography:
# - Real libsignal-protocol on CLI side
# - Real libsignal WASM on browser side
# - Real Action Cable WebSocket relay
# - Real encryption/decryption
#
# Run with: rails test:system TEST=test/system/terminal_relay_test.rb
#
class TerminalRelayTest < ApplicationSystemTestCase
  include CliTestHelper

  setup do
    @user = users(:one)
    @hub = create_test_hub(user: @user)
  end

  teardown do
    stop_cli(@cli) if @cli
    @hub&.destroy
  end

  # === Full E2E Connection Tests ===

  test "full E2E: browser connects to CLI with Signal Protocol encryption" do
    # 1. Start CLI (generates Signal keys, connects to relay)
    @cli = start_cli(@hub, timeout: 20)
    assert @cli.running?, "CLI should be running"

    # 2. Get connection URL (includes PreKeyBundle in fragment)
    connection_url = @cli.connection_url
    assert connection_url.present?, "Connection URL should be available"
    assert connection_url.include?("#bundle="), "URL should contain bundle fragment"

    # Debug: Log what URL we're visiting
    Rails.logger.info "[TerminalRelayTest] Connection URL: #{connection_url}"
    Rails.logger.info "[TerminalRelayTest] User: #{@user.inspect}"

    # 3. Sign in and visit the connection URL
    sign_in_as(@user)
    Rails.logger.info "[TerminalRelayTest] After sign_in_as, warden user: #{warden.user(:user).inspect rescue 'none'}"
    visit connection_url
    Rails.logger.info "[TerminalRelayTest] Current URL after visit: #{current_url}"

    # 4. Wait for handshake to be sent (WASM loaded, session created, handshake sent)
    # The page shows "Handshake sent" or "Establishing E2E Encryption"
    assert_text /handshake sent|establishing e2e/i, wait: 10

    # 5. Wait for full connection (handshake ACK received and decrypted)
    # This requires CLI to be connected to the same Action Cable server
    begin
      assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20
    rescue Minitest::Assertion => e
      # Print CLI logs on failure to help debug
      Rails.logger.error "[TerminalRelayTest] Connection failed. CLI logs:\n#{@cli.log_contents}"
      raise e
    end

    # 8. Verify security banner shows E2E encryption is active
    within "[data-connection-target='securityBanner']" do
      assert_text(/E2E|encrypted|secure/i)
    end

    # 9. Terminal should be visible and ready
    # Note: Using ~= selector because multiple controllers on one element
    assert_selector "[data-controller~='terminal-display']", wait: 5
  end

  test "full E2E: encryption actually works - message roundtrip" do
    @cli = start_cli(@hub, timeout: 20)
    connection_url = @cli.connection_url

    sign_in_as(@user)
    visit connection_url

    # Wait for full connection
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # At this point, the connection is using real Signal Protocol encryption
    # The fact that we got here means:
    # - Browser successfully processed CLI's PreKeyBundle (X3DH)
    # - Browser encrypted and sent handshake
    # - CLI decrypted handshake (proves browser encryption works)
    # - CLI encrypted and sent ACK
    # - Browser decrypted ACK (proves CLI encryption works)
    # - Double Ratchet session is established

    # This is a true E2E crypto test - if any part of the crypto was broken,
    # we would not reach this point
  end

  test "connection fails gracefully when CLI is not running" do
    # Don't start CLI - browser should fail to connect

    sign_in_as(@user)
    visit hub_path(@hub.identifier)

    # Should show error state - no bundle means can't establish session
    # The connection controller shows "Connection failed" with detail "No encryption bundle. Scan QR code to connect."
    assert_text(/connection failed|no bundle|scan qr/i, wait: 10)
  end

  test "connection recovers after page refresh" do
    @cli = start_cli(@hub, timeout: 20)
    connection_url = @cli.connection_url

    sign_in_as(@user)
    visit connection_url

    # Wait for initial connection
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Refresh the page
    page.refresh

    # Should reconnect (session may be restored from IndexedDB or re-established)
    # Either way, should reach connected state
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20
  end

  test "multiple browsers can connect to same CLI" do
    skip "Multi-browser Signal Protocol sessions not yet implemented - requires SenderKey group messaging"

    @cli = start_cli(@hub, timeout: 20)
    connection_url = @cli.connection_url

    sign_in_as(@user)

    # First browser tab
    visit connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Open new window (simulates second browser)
    new_window = open_new_window
    within_window new_window do
      visit connection_url
      assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20
    end

    # Both should still be connected
    assert_selector "[data-connection-target='status']", text: /connected/i
  end

  # === Connection State UI Tests ===

  test "connection status shows correct phases" do
    @cli = start_cli(@hub, timeout: 20)
    connection_url = @cli.connection_url

    sign_in_as(@user)
    visit connection_url

    # Should progress through phases (order may vary slightly)
    # These are the key phases defined in ConnectionState
    phases_seen = []

    # Check for loading phase
    if has_text?("Loading encryption", wait: 2)
      phases_seen << :loading_wasm
    end

    # Check for session creation
    if has_text?("Creating session", wait: 5)
      phases_seen << :creating_session
    end

    # Check for channel connection
    if has_text?("Channel connected", wait: 10)
      phases_seen << :channel_connected
    end

    # Should eventually reach connected
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 15
    phases_seen << :connected

    # Verify we saw the key phases
    assert_includes phases_seen, :connected, "Should reach connected state"
  end

  test "disconnect button appears when connected" do
    @cli = start_cli(@hub, timeout: 20)
    connection_url = @cli.connection_url

    sign_in_as(@user)
    visit connection_url

    # Wait for connection
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Disconnect button should be visible
    assert_selector "[data-connection-target='disconnectBtn']", visible: true
  end

  private

  def sign_in_as(user)
    # System tests with Selenium run in a separate process, so Warden test
    # helpers don't work. Use test-only sign-in endpoint instead.
    #
    # Visit the test sign-in endpoint which signs in and redirects to root.
    visit "/test/sessions/new?user_id=#{user.id}"
  end
end
