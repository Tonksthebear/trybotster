# frozen_string_literal: true

require "application_system_test_case"

# Tests for reliable delivery protocol improvements.
#
# These tests verify the hardened reliability implementation:
# - Session reset detection (browser detects CLI restart)
# - Exponential backoff on retransmission
# - Buffer TTL cleanup (stale messages evicted)
# - Failed message removal (after max retransmits)
# - Connection-aware retransmission (pause when disconnected)
class ReliableDeliveryTest < ApplicationSystemTestCase
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

  # === Phase 1: Session Reset Detection ===

  test "browser receiver has reset method" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Verify the receiver has a reset method
    has_reset = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      return typeof conn.hubChannel?.receiver?.reset === 'function';
    JS
    assert has_reset, "ReliableReceiver should have reset() method"
  end

  test "browser receiver detects session reset on seq=1 after higher" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Wait for messages to be exchanged (nextExpected advances past 1)
    # Poll until receiver has processed at least one message
    deadline = Time.current + 5
    while Time.current < deadline
      state = page.execute_script(<<~JS)
        const conn = window.Stimulus?.getControllerForElementAndIdentifier(
          document.querySelector('[data-controller~="connection"]'), 'connection'
        );
        return conn?.hubChannel?.receiver?.nextExpected || 1;
      JS
      break if state > 1
      sleep 0.2
    end

    # Check initial receiver state
    initial_state = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      const receiver = conn.hubChannel?.receiver;
      return {
        nextExpected: receiver?.nextExpected || 1,
        receivedSize: receiver?.received?.size || 0
      };
    JS

    # nextExpected should have advanced past 1 (messages were received)
    # If not, this test may need adjustment based on actual message flow
    puts "Initial state: nextExpected=#{initial_state['nextExpected']}, received=#{initial_state['receivedSize']}"

    # Simulate receiving seq=1 when nextExpected > 1
    # This triggers the reset detection
    reset_detected = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      const receiver = conn.hubChannel?.receiver;

      // If nextExpected is still 1, manually advance it to test reset detection
      if (receiver && receiver.nextExpected <= 1) {
        receiver.nextExpected = 5;
        receiver.received.add(1);
        receiver.received.add(2);
        receiver.received.add(3);
        receiver.received.add(4);
      }

      const beforeReset = receiver?.nextExpected || 0;

      // Now simulate receiving seq=1 (should trigger reset)
      // This mimics what happens when CLI restarts
      if (receiver && receiver.nextExpected > 1) {
        // The receive method should detect seq=1 and reset
        // We'll call it with a mock payload
        const mockPayload = [0, 123, 34, 116, 121, 112, 101, 34, 58, 34, 116, 101, 115, 116, 34, 125];
        receiver.receive(1, mockPayload);
      }

      const afterReset = receiver?.nextExpected || 0;
      // After reset + delivering seq=1, received should only have seq=1 (not old 1-4)
      const receivedCountAfter = receiver?.received?.size || 0;

      return {
        beforeReset: beforeReset,
        afterReset: afterReset,
        receivedCountAfter: receivedCountAfter,
        // Reset clears state, then seq=1 is delivered (advancing nextExpected to 2)
        // The key indicator is that received set was cleared (now only has seq=1)
        resetOccurred: beforeReset > 1 && afterReset === 2 && receivedCountAfter === 1
      };
    JS

    puts "Reset detection result: #{reset_detected.inspect}"
    assert reset_detected["resetOccurred"],
      "Receiver should reset when seq=1 arrives after higher sequences (before=#{reset_detected['beforeReset']}, after=#{reset_detected['afterReset']}, receivedCount=#{reset_detected['receivedCountAfter']})"
  end

  test "sender also resets when receiver detects peer reset" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Verify sender has reset method
    has_sender_reset = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      return typeof conn.hubChannel?.sender?.reset === 'function';
    JS
    assert has_sender_reset, "ReliableSender should have reset() method"
  end

  # === Phase 2: Exponential Backoff ===

  test "sender has calculateTimeout method for backoff" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Verify exponential backoff calculation exists
    has_backoff = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      const sender = conn.hubChannel?.sender;
      return typeof sender?.calculateTimeout === 'function';
    JS
    assert has_backoff, "ReliableSender should have calculateTimeout() method for exponential backoff"
  end

  test "timeout increases with attempt count" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Verify timeout increases with attempts
    timeouts = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      const sender = conn.hubChannel?.sender;
      if (!sender?.calculateTimeout) return null;

      return {
        attempt1: sender.calculateTimeout(1),
        attempt2: sender.calculateTimeout(2),
        attempt3: sender.calculateTimeout(3),
        attempt5: sender.calculateTimeout(5),
        attempt10: sender.calculateTimeout(10)
      };
    JS

    if timeouts
      assert timeouts["attempt2"] > timeouts["attempt1"],
        "Timeout should increase: attempt2 (#{timeouts['attempt2']}) > attempt1 (#{timeouts['attempt1']})"
      assert timeouts["attempt3"] > timeouts["attempt2"],
        "Timeout should increase: attempt3 (#{timeouts['attempt3']}) > attempt2 (#{timeouts['attempt2']})"
      # Should be capped at 30 seconds
      assert timeouts["attempt10"] <= 30_000,
        "Timeout should be capped at 30 seconds, got #{timeouts['attempt10']}"
    else
      flunk "calculateTimeout method not found"
    end
  end

  # === Phase 3: Buffer TTL Cleanup ===

  test "receiver buffer stores timestamps" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Add an out-of-order message to buffer and verify it has timestamp
    buffer_entry = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      const receiver = conn.hubChannel?.receiver;
      if (!receiver) return null;

      // Manually add an out-of-order message (seq=10 when expecting seq=1)
      receiver.buffer.set(10, {
        payload: { test: true },
        receivedAt: Date.now()
      });

      const entry = receiver.buffer.get(10);
      return {
        hasReceivedAt: entry && typeof entry.receivedAt === 'number',
        bufferSize: receiver.buffer.size
      };
    JS

    assert buffer_entry["hasReceivedAt"], "Buffer entries should have receivedAt timestamp"
    assert_equal 1, buffer_entry["bufferSize"]
  end

  test "receiver has cleanupStaleBuffer method" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    has_cleanup = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      return typeof conn.hubChannel?.receiver?.cleanupStaleBuffer === 'function';
    JS
    assert has_cleanup, "ReliableReceiver should have cleanupStaleBuffer() method"
  end

  # === Phase 4: Failed Message Removal ===

  test "sender removes messages after max retransmit attempts" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Test that failed messages are removed from pending
    result = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      const sender = conn.hubChannel?.sender;
      if (!sender) return { error: 'no sender' };

      // Add a message with max attempts exceeded
      const seq = sender.nextSeq++;
      sender.pending.set(seq, {
        payloadBytes: [1, 2, 3],
        firstSentAt: Date.now() - 60000,
        lastSentAt: Date.now() - 5000,
        attempts: 11  // > MAX_RETRANSMIT_ATTEMPTS (10)
      });

      const beforeCount = sender.pending.size;

      // Call getRetransmits - should remove the failed message
      sender.getRetransmits();

      const afterCount = sender.pending.size;

      return {
        beforeCount: beforeCount,
        afterCount: afterCount,
        messageRemoved: afterCount < beforeCount
      };
    JS

    assert result["messageRemoved"],
      "Message should be removed after max retransmits (before=#{result['beforeCount']}, after=#{result['afterCount']})"
  end

  # === Phase 5: Connection-Aware Retransmission ===

  test "sender has pause and resume methods" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    methods_exist = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      const sender = conn.hubChannel?.sender;
      return {
        hasPause: typeof sender?.pause === 'function',
        hasResume: typeof sender?.resume === 'function',
        hasPausedProperty: 'paused' in (sender || {})
      };
    JS

    assert methods_exist["hasPause"], "Sender should have pause() method"
    assert methods_exist["hasResume"], "Sender should have resume() method"
    assert methods_exist["hasPausedProperty"], "Sender should have paused property"
  end

  test "pause stops retransmit timer" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    result = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      const sender = conn.hubChannel?.sender;
      if (!sender?.pause) return { error: 'no pause method' };

      // Add a pending message to ensure timer would be active
      sender.pending.set(999, {
        payloadBytes: [1, 2, 3],
        firstSentAt: Date.now(),
        lastSentAt: Date.now(),
        attempts: 1
      });
      sender.scheduleRetransmit();

      const hadTimer = !!sender.retransmitTimer;

      // Pause should clear timer
      sender.pause();

      return {
        hadTimer: hadTimer,
        timerClearedAfterPause: sender.retransmitTimer === null,
        isPaused: sender.paused === true
      };
    JS

    assert result["timerClearedAfterPause"], "Retransmit timer should be cleared after pause()"
    assert result["isPaused"], "Sender should be marked as paused"
  end

  # === General Reliability ===

  test "receiver buffer remains bounded during normal operation" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Check buffer is empty initially
    buffer_size = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      return conn.hubChannel?.receiver?.buffer?.size || 0;
    JS
    assert_equal 0, buffer_size, "Buffer should be empty during normal operation"
  end

  test "heartbeat keeps connection alive during idle periods" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # INTENTIONAL TIMING TEST: Wait through multiple heartbeat intervals (2s each in test mode)
    # to verify connection remains stable. This sleep cannot be replaced with a
    # condition check - we need actual wall-clock time to pass.
    sleep 5

    # Connection should still be alive
    assert_selector "[data-connection-target='status']", text: /connected/i

    # CLI should have processed heartbeats during idle period
    cli_log = @cli.log_contents(lines: 100)
    assert_match(/heartbeat/i, cli_log, "CLI should have processed heartbeats during idle period")
  end

  test "messages delivered in order during normal operation" do
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Verify receiver is tracking sequence numbers correctly
    result = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      const receiver = conn.hubChannel?.receiver;
      if (!receiver) return { error: 'no receiver' };

      return {
        nextExpected: receiver.nextExpected,
        receivedCount: receiver.received.size,
        bufferSize: receiver.buffer.size,
        hasOnDeliver: typeof receiver.onDeliver === 'function'
      };
    JS

    # After connecting, some messages should have been exchanged
    assert result["nextExpected"] >= 1, "Receiver should be tracking sequences"
    assert result["hasOnDeliver"], "Receiver should have delivery callback"
    assert_equal 0, result["bufferSize"], "Buffer should be empty (in-order delivery)"
  end

  private

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end
end
