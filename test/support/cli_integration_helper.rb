# frozen_string_literal: true

# Shared helpers for system tests that drive a running CLI and exercise the
# full pairing + WebRTC + readiness flow. Centralizes the `sign_in_as` /
# `sign_in_and_connect` / `revisit_pairing_url_if_needed` helpers that were
# previously duplicated across test/system/*.rb.
#
# Relies on helpers provided by ApplicationSystemTestCase:
#   - SpaSystemHelper (complete_pairing_for, assert_sidebar_webrtc_connected, ...)
#   - SystemReadinessHelpers (wait_for_hub_ready, wait_for_surface_ready, ...)
module CliIntegrationHelper
  # Visit the test-only bypass login endpoint. All system tests use this to
  # establish a Warden session without driving the full GitHub OAuth flow.
  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end

  # Drive the browser through the CLI's connection URL (carrying the E2E key
  # bundle in the fragment), complete the pairing confirmation, and land on
  # the hub page with WebRTC up.
  #
  # Defaults match the common case: `@user`, `@hub`, `@cli` are set by the
  # test's setup block and the CLI has already been started. Options cover
  # the two legitimate variations observed across the suite:
  #
  #   prewarm_hub_page: visit hub_path BEFORE the pairing URL (file_input).
  #   retry_if_stale: run revisit_pairing_url_if_needed after pairing to
  #     recover from "pairing_needed" state on re-pair scenarios.
  #   gate_hub_ready: block on wait_for_hub_ready at the end. Default true.
  def sign_in_and_connect(
    user: @user,
    hub: @hub,
    cli: @cli,
    prewarm_hub_page: false,
    webrtc_timeout: 30,
    retry_if_stale: false,
    gate_hub_ready: true
  )
    url = cli.connection_url
    assert url.present?, "CLI should produce a connection URL"

    sign_in_as(user)
    visit hub_path(hub) if prewarm_hub_page
    visit url

    complete_pairing_for(hub, pairing_url: url)
    revisit_pairing_url_if_needed(url, hub: hub) if retry_if_stale

    assert_sidebar_webrtc_connected(wait: webrtc_timeout)
    wait_for_hub_ready if gate_hub_ready
  end

  # If the SPA has dropped into a "pairing_needed" state (stale session or a
  # lost Olm handshake), revisit the pairing URL once to re-drive the flow.
  # Otherwise a no-op. Used by tests that do a pre-pair visit to the hub page
  # and by tests that stop/restart the CLI mid-test.
  def revisit_pairing_url_if_needed(url, hub: @hub)
    return unless page.has_button?("Start pairing", wait: 2) ||
      page.has_selector?(
        "#{SpaSystemHelper::SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='pairing_needed']",
        wait: 2
      )

    visit url
    complete_pairing_for(hub, pairing_url: url)
  end
end
