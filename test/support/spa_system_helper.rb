# frozen_string_literal: true

module SpaSystemHelper
  SIDEBAR_CONNECTION_STATUS_SELECTOR = "[data-testid='sidebar-connection-status']"

  def assert_pairing_ready(wait: 15, hub: nil, pairing_url: nil)
    pairing_url ||= current_url if current_url.include?("/pairing")

    return if page.has_selector?("[data-testid='pairing-ready']", wait: 2)
    return if page.has_selector?("[data-testid='pairing-success']", wait: 2)
    return if page.has_selector?(
      "#{SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='direct'], " \
      "#{SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='relay']",
      wait: 2
    )

    if page.has_button?("Start pairing", wait: 2)
      click_button "Start pairing"
    end

    return if page.has_selector?("[data-testid='pairing-ready']", wait: 2)
    return if page.has_selector?("[data-testid='pairing-success']", wait: 2)
    return if pairing_url && load_pairing_link_into_paste_input(pairing_url)

    if pairing_url
      # When the SPA is already mounted on /pairing in paste mode, navigating to
      # the same route with only a restored hash does not remount PairingPage.
      # Force a route change so the fragment is parsed on the next mount.
      visit hub_path(hub) if hub && page.current_path.include?("/pairing")
      visit pairing_url
      click_button "Start pairing" if page.has_button?("Start pairing", wait: 2)
      return if load_pairing_link_into_paste_input(pairing_url)
    elsif hub
      visit hub_path(hub)
      click_button "Start pairing" if page.has_button?("Start pairing", wait: 2)
    end

    assert_selector "[data-testid='pairing-ready']", wait: wait
  end

  def assert_pairing_success(wait: 15)
    assert_selector "[data-testid='pairing-success']", wait: wait
  end

  def complete_pairing_for(hub, wait: 15, pairing_url: nil)
    return if page.has_selector?(
      "#{SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='direct'], " \
      "#{SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='relay']",
      wait: 2
    )

    assert_pairing_ready(wait: wait, hub:, pairing_url:)

    if page.has_button?("Complete Pairing", wait: 2)
      click_button "Complete Pairing", wait: 10
      assert_pairing_success(wait: wait)
    end

    return if !page.current_path.include?("/pairing") &&
      page.has_selector?(SIDEBAR_CONNECTION_STATUS_SELECTOR, wait: wait)

    visit hub_path(hub)
    assert_sidebar_connection_status(wait: wait)
  end

  def assert_sidebar_connection_status(connection: nil, browser: nil, hub: nil, wait: 15)
    assert_selector sidebar_connection_status_selector(connection:, browser:, hub:), wait: wait
  end

  def assert_sidebar_webrtc_connected(wait: 30)
    assert_selector(
      "#{SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='direct'], " \
      "#{SIDEBAR_CONNECTION_STATUS_SELECTOR}[data-connection-state='relay']",
      wait: wait
    )
  end

  def sidebar_connection_state(wait: 15)
    find(SIDEBAR_CONNECTION_STATUS_SELECTOR, wait: wait)["data-connection-state"]
  end

  def sidebar_connection_status_selector(connection: nil, browser: nil, hub: nil)
    selector = SIDEBAR_CONNECTION_STATUS_SELECTOR.dup
    selector << "[data-connection-state='#{connection}']" if connection
    selector << "[data-browser-status='#{browser}']" if browser
    selector << "[data-hub-status='#{hub}']" if hub
    selector
  end

  def load_pairing_link_into_paste_input(pairing_url)
    return false unless page.has_selector?("[data-testid='pairing-paste']", wait: 2)

    input = find("input[placeholder='Paste connection link here...']", wait: 2)
    input.click
    page.execute_script(<<~JS, input.native, pairing_url)
      const input = arguments[0];
      const value = arguments[1];

      input.value = value;
      input.dispatchEvent(new Event('input', { bubbles: true }));
      input.dispatchEvent(new Event('paste', { bubbles: true }));
    JS

    page.has_selector?("[data-testid='pairing-ready']", wait: 5)
  end
end
