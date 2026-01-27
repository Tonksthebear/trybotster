# frozen_string_literal: true

require "application_system_test_case"

# Tests for correct channel routing in browser JavaScript.
#
# Verifies the architecture:
# - HubChannel: hub-level commands (list agents, select, create) - subscribed on page load
# - TerminalRelayChannel: PTY I/O (input, output, resize) - subscribed on demand via connectToPty()
#
# This ensures PTY I/O doesn't route through the hub channel which
# would be architecturally wrong (hub should not be in the PTY data path).
class ChannelRoutingTest < ApplicationSystemTestCase
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

  # === Channel Architecture Tests ===

  test "sendInput routes through terminal channel not hub channel" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # Connect to PTY to establish terminal channel (on-demand architecture)
    connect_to_pty_in_browser

    # Verify sendInput uses sendTerminalMessage internally
    routing_check = page.execute_script(<<~JS)
      const hubConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="hub-connection"]'), 'hub-connection'
      );
      const termConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="terminal-connection"]'), 'terminal-connection'
      );

      // Spy on the channel send methods
      let hubChannelCalled = false;
      let terminalChannelCalled = false;

      const originalHubSend = hubConn.hubChannel?.send?.bind(hubConn.hubChannel);
      const originalTerminalSend = termConn.terminalChannel?.send?.bind(termConn.terminalChannel);

      if (hubConn.hubChannel?.send) {
        hubConn.hubChannel.send = async (msg) => {
          hubChannelCalled = true;
          return originalHubSend ? originalHubSend(msg) : false;
        };
      }

      if (termConn.terminalChannel?.send) {
        termConn.terminalChannel.send = async (msg) => {
          terminalChannelCalled = true;
          return originalTerminalSend ? originalTerminalSend(msg) : false;
        };
      }

      // Call sendInput on terminal connection
      termConn.sendInput('test');

      // Small delay to let async complete
      await new Promise(r => setTimeout(r, 50));

      // Restore originals
      if (originalHubSend) hubConn.hubChannel.send = originalHubSend;
      if (originalTerminalSend) termConn.terminalChannel.send = originalTerminalSend;

      return {
        hubChannelCalled,
        terminalChannelCalled,
        hasTerminalChannel: !!termConn.terminalChannel,
        hasHubChannel: !!hubConn.hubChannel
      };
    JS

    assert routing_check["hasTerminalChannel"], "Should have terminal channel"
    assert routing_check["hasHubChannel"], "Should have hub channel"
    assert routing_check["terminalChannelCalled"],
      "sendInput should route through terminal channel"
    refute routing_check["hubChannelCalled"],
      "sendInput should NOT route through hub channel"
  end

  test "sendResize routes through terminal channel not hub channel" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # Connect to PTY to establish terminal channel (on-demand architecture)
    connect_to_pty_in_browser

    # Verify sendResize uses sendTerminalMessage internally
    routing_check = page.execute_script(<<~JS)
      const hubConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="hub-connection"]'), 'hub-connection'
      );
      const termConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="terminal-connection"]'), 'terminal-connection'
      );

      // Spy on the channel send methods
      let hubChannelCalled = false;
      let terminalChannelCalled = false;

      const originalHubSend = hubConn.hubChannel?.send?.bind(hubConn.hubChannel);
      const originalTerminalSend = termConn.terminalChannel?.send?.bind(termConn.terminalChannel);

      if (hubConn.hubChannel?.send) {
        hubConn.hubChannel.send = async (msg) => {
          hubChannelCalled = true;
          return originalHubSend ? originalHubSend(msg) : false;
        };
      }

      if (termConn.terminalChannel?.send) {
        termConn.terminalChannel.send = async (msg) => {
          terminalChannelCalled = true;
          return originalTerminalSend ? originalTerminalSend(msg) : false;
        };
      }

      // Call sendResize on terminal connection
      termConn.sendResize(80, 24);

      // Small delay to let async complete
      await new Promise(r => setTimeout(r, 50));

      // Restore originals
      if (originalHubSend) hubConn.hubChannel.send = originalHubSend;
      if (originalTerminalSend) termConn.terminalChannel.send = originalTerminalSend;

      return {
        hubChannelCalled,
        terminalChannelCalled
      };
    JS

    assert routing_check["terminalChannelCalled"],
      "sendResize should route through terminal channel"
    refute routing_check["hubChannelCalled"],
      "sendResize should NOT route through hub channel"
  end

  test "send (hub commands) routes through hub channel" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # Verify send() routes through hub channel
    routing_check = page.execute_script(<<~JS)
      const hubConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="hub-connection"]'), 'hub-connection'
      );

      // Spy on the hub channel send method
      let hubChannelCalled = false;

      const originalHubSend = hubConn.hubChannel?.send?.bind(hubConn.hubChannel);

      if (hubConn.hubChannel?.send) {
        hubConn.hubChannel.send = async (msg) => {
          hubChannelCalled = true;
          return originalHubSend ? originalHubSend(msg) : false;
        };
      }

      // Call send (hub-level command)
      hubConn.send('list_agents');

      // Small delay to let async complete
      await new Promise(r => setTimeout(r, 50));

      // Restore originals
      if (originalHubSend) hubConn.hubChannel.send = originalHubSend;

      return {
        hubChannelCalled
      };
    JS

    assert routing_check["hubChannelCalled"],
      "send() should route through hub channel for agent commands"
  end

  test "sendTerminalMessage has correct signature" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # Navigate to agent page to access terminal-connection controller
    navigate_to_agent_page

    # Verify terminal methods exist on terminal-connection and hub methods on hub-connection
    api_check = page.execute_script(<<~JS)
      const hubConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="hub-connection"]'), 'hub-connection'
      );
      const termConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="terminal-connection"]'), 'terminal-connection'
      );

      return {
        hasSendTerminalMessage: typeof termConn.sendTerminalMessage === 'function',
        hasSendInput: typeof termConn.sendInput === 'function',
        hasSendResize: typeof termConn.sendResize === 'function',
        hasHubSend: typeof hubConn.send === 'function'
      };
    JS

    assert api_check["hasSendTerminalMessage"], "Terminal connection should have sendTerminalMessage method"
    assert api_check["hasSendInput"], "Terminal connection should have sendInput method"
    assert api_check["hasSendResize"], "Terminal connection should have sendResize method"
    assert api_check["hasHubSend"], "Hub connection should have send method"
  end

  test "terminal channel is separate from hub channel" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # Connect to PTY to establish terminal channel (on-demand architecture)
    connect_to_pty_in_browser

    # Verify the channels are on distinct controllers
    channel_check = page.execute_script(<<~JS)
      const hubConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="hub-connection"]'), 'hub-connection'
      );
      const termConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="terminal-connection"]'), 'terminal-connection'
      );

      return {
        hubChannel: !!hubConn.hubChannel,
        terminalChannel: !!termConn.terminalChannel,
        hubSubscription: !!hubConn.hubSubscription,
        terminalSubscription: !!termConn.terminalSubscription
      };
    JS

    assert channel_check["hubChannel"], "Hub connection should have hub channel"
    assert channel_check["terminalChannel"], "Terminal connection should have terminal channel"
    assert channel_check["hubSubscription"], "Hub connection should have hub subscription"
    assert channel_check["terminalSubscription"], "Terminal connection should have terminal subscription"
  end

  # === Browser -> PTY I/O Flow E2E Tests ===
  #
  # These tests verify the complete flow from browser to PTY and back:
  # - Browser selects agent -> TerminalRelayChannel subscribed
  # - Browser types -> input reaches CLI -> PTY
  # - PTY output -> CLI -> TerminalRelayChannel -> browser

  test "browser selecting agent subscribes to terminal relay channel" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # Navigate to agent page (terminal-connection only exists there)
    navigate_to_agent_page

    # Verify terminal channel does NOT exist before PTY connection (on-demand architecture)
    before_state = page.execute_script(<<~JS)
      const termConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="terminal-connection"]'), 'terminal-connection'
      );
      return {
        hasTerminalChannel: !!termConn?.terminalChannel,
        hasTerminalSubscription: !!termConn?.terminalSubscription
      };
    JS

    refute before_state["hasTerminalChannel"], "Should NOT have terminal channel before connectToPty"
    refute before_state["hasTerminalSubscription"], "Should NOT have terminal subscription before connectToPty"

    # Connect to PTY (triggers on-demand terminal channel subscription)
    connect_to_pty_in_browser

    # Verify terminal channel NOW exists after connectToPty
    after_state = page.execute_script(<<~JS)
      const termConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="terminal-connection"]'), 'terminal-connection'
      );
      return {
        hasTerminalChannel: !!termConn.terminalChannel,
        hasTerminalSubscription: !!termConn.terminalSubscription
      };
    JS

    assert after_state["hasTerminalChannel"], "Should have terminal channel after connectToPty"
    assert after_state["hasTerminalSubscription"], "Should have terminal subscription after connectToPty"
  end

  test "terminal output appears in browser terminal" do
    skip "Requires agent with spawned PTY - tested in full integration suite"
  end

  test "browser input reaches PTY when agent is selected" do
    skip "Requires agent with spawned PTY - tested in full integration suite"
  end

  test "browser resize affects PTY dimensions" do
    skip "Requires agent with spawned PTY - tested in full integration suite"
  end

  test "switching agents changes terminal channel subscription" do
    skip "Requires multiple agents - tested in full integration suite"
  end

  private

  # Navigate to agent terminal page (which has terminal-connection controller).
  # The hub landing page only has hub-connection; terminal-connection is on the agent page.
  def navigate_to_agent_page(agent_index = 0)
    hub_id = @hub.id
    visit "/hubs/#{hub_id}/agents/#{agent_index}"
    assert_selector "[data-controller~='terminal-connection']", wait: 5
    # Wait for hub-connection to reconnect after Turbo navigation
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20
  end

  def connect_to_pty_in_browser(agent_index = 0, pty_index = 0)
    # Ensure we're on the agent page (which has terminal-connection controller)
    unless page.has_css?("[data-controller~='terminal-connection']", wait: 0)
      navigate_to_agent_page(agent_index)
    end

    page.execute_script(<<~JS)
      const termConn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="terminal-connection"]'), 'terminal-connection'
      );
      termConn.connectToPty(#{agent_index}, #{pty_index});
    JS
    sleep 1 # Wait for ActionCable subscription to establish
  end

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end
end
