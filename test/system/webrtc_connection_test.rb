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
    pair_browser_with_cli(@cli)

    # Browser section should reach "connected" (ActionCable signaling up)
    assert_selector(
      "[data-connection-status-target='browserSection'][data-status='connected']",
      wait: 30
    )

    # Connection section should reach "direct" or "relay" (WebRTC data channel open)
    assert_webrtc_connected

    # Hub section should show "online" (CLI heartbeating)
    assert_selector(
      "[data-connection-status-target='hubSection'][data-status='online']",
      wait: 30
    )
  end

  test "browser and hub ready state triggers a WebRTC attempt" do
    @cli = start_cli(@hub)
    pair_browser_with_cli(@cli)

    # This guards the exact gate semantics for the status badges: once the
    # browser is connected to Rails and the hub is online, the middle badge
    # must at least enter a live WebRTC attempt.
    assert_selector(
      "[data-connection-status-target='browserSection'][data-status='connected']",
      wait: 30
    )
    assert_selector(
      "[data-connection-status-target='hubSection'][data-status='online']",
      wait: 30
    )

    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='connecting'], " \
      "[data-connection-status-target='connectionSection'][data-state='direct'], " \
      "[data-connection-status-target='connectionSection'][data-state='relay']",
      wait: 10
    )
  end

  test "crypto SharedWorker reaches ready and init on hub page" do
    sign_in_as(@user)
    visit hub_path(@hub)

    # SharedWorker bootstrap is part of the real browser connection path.
    # Prove ready -> init explicitly so regressions fail before later WebRTC
    # assertions become ambiguous.
    result = page.evaluate_async_script(<<~JS)
      const done = arguments[arguments.length - 1];
      const workerMeta = document.querySelector('meta[name="crypto-worker-url"]');
      const wasmJsMeta = document.querySelector('meta[name="crypto-wasm-js-url"]');
      const wasmBinaryMeta = document.querySelector('meta[name="crypto-wasm-binary-url"]');

      if (!workerMeta?.content || !wasmJsMeta?.content) {
        done({ ok: false, error: "missing worker meta tags" });
        return;
      }

      let settled = false;
      const finish = (payload) => {
        if (settled) return;
        settled = true;
        done(payload);
      };

      const timer = setTimeout(() => {
        finish({ ok: false, error: "timeout waiting for SharedWorker init" });
      }, 30000);

      try {
        const worker = new SharedWorker(workerMeta.content, { name: "vodozemac-crypto-test" });
        worker.port.onmessage = (event) => {
          const data = event.data || {};

          if (data.event === "ready") {
            worker.port.postMessage({
              id: 424242,
              action: "init",
              wasmJsUrl: wasmJsMeta.content,
              wasmBinaryUrl: wasmBinaryMeta?.content || null,
            });
            return;
          }

          if (data.id === 424242) {
            clearTimeout(timer);
            finish({ ok: !!data.success, error: data.error || null, result: data.result || null });
          }
        };
        worker.port.start();
      } catch (error) {
        clearTimeout(timer);
        finish({ ok: false, error: String(error) });
      }
    JS

    assert_equal true, result["ok"], "Expected SharedWorker init to succeed, got: #{result.inspect}"
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
    complete_pairing

    # Verify connection establishes via the pairing bundle
    assert_webrtc_connected
  end

  test "connection survives page refresh" do
    @cli = start_cli(@hub)
    pair_browser_with_cli(@cli)

    # Wait for full connection
    assert_webrtc_connected
    paired_hub_url = current_url

    # Refresh the page. The reconnect path should recover without manual
    # intervention while the browser session remains alive.
    visit paired_hub_url

    # Connection should re-establish without manual intervention.
    assert_selector(
      "[data-connection-status-target='browserSection'][data-status='connected']",
      wait: 30
    )

    assert_webrtc_connected

    assert_selector(
      "[data-connection-status-target='hubSection'][data-status='online']",
      wait: 30
    )
  end

  test "connection re-establishes after client-side navigation away and back" do
    @cli = start_cli(@hub)
    pair_browser_with_cli(@cli)

    # Wait for full WebRTC connection
    assert_webrtc_connected
    paired_hub_url = current_url

    # Navigate away, which releases the active connections and starts the grace period.
    visit settings_path

    # Navigate back to the hub and reacquire the same connections without storming.
    visit paired_hub_url

    # Connection should re-establish cleanly within a reasonable timeout.
    # Before the fix, this would never stabilize — the browser would loop
    # creating peer connections that the CLI rejects ("Connection in progress").
    assert_webrtc_connected(wait: 15)

    assert_selector(
      "[data-connection-status-target='hubSection'][data-status='online']",
      wait: 15
    )
  end

  test "paired browser reconnects after hub reboot without rescanning" do
    @cli = start_cli(@hub)
    pair_browser_with_cli(@cli)

    assert_webrtc_connected

    preserved_temp_dir = @cli.temp_dir
    stop_cli(@cli, preserve_temp_dir: true)
    @cli = nil

    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='disconnected'], " \
      "[data-connection-status-target='connectionSection'][data-state='expired'], " \
      "[data-connection-status-target='connectionSection'][data-state='unpaired']",
      wait: 20
    )

    @cli = start_cli(@hub, temp_dir: preserved_temp_dir)

    visit current_url

    assert_selector(
      "[data-connection-status-target='browserSection'][data-status='connected']",
      wait: 30
    )
    assert_webrtc_connected(wait: 30)
    assert_selector(
      "[data-connection-status-target='hubSection'][data-status='online']",
      wait: 30
    )
  end

  test "without CLI connection shows appropriate state" do
    sign_in_as(@user)
    visit hub_path(@hub)

    # Browser section should still show "connected" because it only reflects
    # the browser's ActionCable connection to Rails.
    assert_selector(
      "[data-connection-status-target='browserSection'][data-status='connected']",
      wait: 10
    )

    # Hub section should show "offline" since CLI is not running
    assert_selector(
      "[data-connection-status-target='hubSection'][data-status='offline']",
      wait: 10
    )

    # Connection section should not look "connecting" just because browser
    # signaling is up. Without both sides ready for WebRTC, it stays
    # disconnected or unpaired.
    connection_section = find("[data-connection-status-target='connectionSection']", wait: 10)
    state = connection_section["data-state"]
    assert_includes(
      %w[unpaired disconnected],
      state,
      "Without CLI, connection state should be unpaired or disconnected (got #{state})"
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

  # Visit connection URL, complete the pairing confirmation, and land on the hub page.
  def pair_browser_with_cli(cli)
    url = cli.connection_url
    assert url.present?, "CLI should produce a connection URL"

    sign_in_as(@user)
    visit url
    complete_pairing
  end

  # Click "Complete Secure Pairing" and wait for redirect to hub page.
  # The pairing page parses the bundle from the URL fragment, shows a
  # confirmation button, then redirects to the hub after session creation.
  def complete_pairing
    click_button "Complete Pairing", wait: 10
    assert_selector "[data-pairing-target='success']:not(.hidden)", wait: 15
    visit hub_path(@hub)
    assert_selector "[data-connection-status-target='connectionSection']", wait: 15
  end

  # Assert WebRTC data channel is connected (direct P2P or TURN relay).
  def assert_webrtc_connected(wait: 30)
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='direct'], " \
      "[data-connection-status-target='connectionSection'][data-state='relay']",
      wait: wait
    )
  end
end
