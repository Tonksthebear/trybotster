# frozen_string_literal: true

require "application_system_test_case"

# Tests for correct channel routing in browser JavaScript.
#
# Verifies the architecture:
# - HubChannel: hub-level commands (list agents, select, create)
# - TerminalRelayChannel: PTY I/O (input, output, resize)
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
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Verify sendInput uses sendTerminalMessage internally
    routing_check = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );

      // Spy on the channel send methods
      let hubChannelCalled = false;
      let terminalChannelCalled = false;

      const originalHubSend = conn.hubChannel?.send?.bind(conn.hubChannel);
      const originalTerminalSend = conn.terminalChannel?.send?.bind(conn.terminalChannel);

      if (conn.hubChannel?.send) {
        conn.hubChannel.send = async (msg) => {
          hubChannelCalled = true;
          return originalHubSend ? originalHubSend(msg) : false;
        };
      }

      if (conn.terminalChannel?.send) {
        conn.terminalChannel.send = async (msg) => {
          terminalChannelCalled = true;
          return originalTerminalSend ? originalTerminalSend(msg) : false;
        };
      }

      // Call sendInput
      conn.sendInput('test');

      // Small delay to let async complete
      await new Promise(r => setTimeout(r, 50));

      // Restore originals
      if (originalHubSend) conn.hubChannel.send = originalHubSend;
      if (originalTerminalSend) conn.terminalChannel.send = originalTerminalSend;

      return {
        hubChannelCalled,
        terminalChannelCalled,
        hasTerminalChannel: !!conn.terminalChannel,
        hasHubChannel: !!conn.hubChannel
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
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Verify sendResize uses sendTerminalMessage internally
    routing_check = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );

      // Spy on the channel send methods
      let hubChannelCalled = false;
      let terminalChannelCalled = false;

      const originalHubSend = conn.hubChannel?.send?.bind(conn.hubChannel);
      const originalTerminalSend = conn.terminalChannel?.send?.bind(conn.terminalChannel);

      if (conn.hubChannel?.send) {
        conn.hubChannel.send = async (msg) => {
          hubChannelCalled = true;
          return originalHubSend ? originalHubSend(msg) : false;
        };
      }

      if (conn.terminalChannel?.send) {
        conn.terminalChannel.send = async (msg) => {
          terminalChannelCalled = true;
          return originalTerminalSend ? originalTerminalSend(msg) : false;
        };
      }

      // Call sendResize
      conn.sendResize(80, 24);

      // Small delay to let async complete
      await new Promise(r => setTimeout(r, 50));

      // Restore originals
      if (originalHubSend) conn.hubChannel.send = originalHubSend;
      if (originalTerminalSend) conn.terminalChannel.send = originalTerminalSend;

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
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Verify send() routes through hub channel
    routing_check = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );

      // Spy on the channel send methods
      let hubChannelCalled = false;
      let terminalChannelCalled = false;

      const originalHubSend = conn.hubChannel?.send?.bind(conn.hubChannel);
      const originalTerminalSend = conn.terminalChannel?.send?.bind(conn.terminalChannel);

      if (conn.hubChannel?.send) {
        conn.hubChannel.send = async (msg) => {
          hubChannelCalled = true;
          return originalHubSend ? originalHubSend(msg) : false;
        };
      }

      if (conn.terminalChannel?.send) {
        conn.terminalChannel.send = async (msg) => {
          terminalChannelCalled = true;
          return originalTerminalSend ? originalTerminalSend(msg) : false;
        };
      }

      // Call send (hub-level command)
      conn.send('list_agents');

      // Small delay to let async complete
      await new Promise(r => setTimeout(r, 50));

      // Restore originals
      if (originalHubSend) conn.hubChannel.send = originalHubSend;
      if (originalTerminalSend) conn.terminalChannel.send = originalTerminalSend;

      return {
        hubChannelCalled,
        terminalChannelCalled
      };
    JS

    assert routing_check["hubChannelCalled"],
      "send() should route through hub channel for agent commands"
    refute routing_check["terminalChannelCalled"],
      "send() should NOT route through terminal channel"
  end

  test "sendTerminalMessage has correct signature" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Verify sendTerminalMessage exists and works correctly
    api_check = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );

      return {
        hasSendTerminalMessage: typeof conn.sendTerminalMessage === 'function',
        hasSendInput: typeof conn.sendInput === 'function',
        hasSendResize: typeof conn.sendResize === 'function',
        hasSend: typeof conn.send === 'function'
      };
    JS

    assert api_check["hasSendTerminalMessage"], "Should have sendTerminalMessage method"
    assert api_check["hasSendInput"], "Should have sendInput method"
    assert api_check["hasSendResize"], "Should have sendResize method"
    assert api_check["hasSend"], "Should have send method"
  end

  test "terminal channel is separate from hub channel" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Verify the channels are distinct objects
    channel_check = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );

      return {
        hubChannel: !!conn.hubChannel,
        terminalChannel: !!conn.terminalChannel,
        channelsAreDifferent: conn.hubChannel !== conn.terminalChannel,
        hubSubscription: !!conn.hubSubscription,
        terminalSubscription: !!conn.terminalSubscription,
        subscriptionsAreDifferent: conn.hubSubscription !== conn.terminalSubscription
      };
    JS

    assert channel_check["hubChannel"], "Should have hub channel"
    assert channel_check["terminalChannel"], "Should have terminal channel"
    assert channel_check["channelsAreDifferent"], "Hub and terminal channels should be different objects"
    assert channel_check["hubSubscription"], "Should have hub subscription"
    assert channel_check["terminalSubscription"], "Should have terminal subscription"
    assert channel_check["subscriptionsAreDifferent"], "Hub and terminal subscriptions should be different"
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
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Verify terminal channel is subscribed after connection
    channel_state = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );

      return {
        hasTerminalChannel: !!conn.terminalChannel,
        hasTerminalSubscription: !!conn.terminalSubscription,
        terminalChannelConnected: conn.terminalChannel?.isConnected?.() ?? false
      };
    JS

    assert channel_state["hasTerminalChannel"], "Should have terminal channel after connection"
    assert channel_state["hasTerminalSubscription"], "Should have terminal subscription after connection"
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

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end
end
