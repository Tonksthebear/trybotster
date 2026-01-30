# frozen_string_literal: true

# Generic polling helper for test synchronization.
#
# Usage:
#
#   # Basic usage - raises on timeout
#   wait_until(timeout: 10) { hub.reload.last_seen_at.present? }
#
#   # With custom error message
#   wait_until(timeout: 10, message: "Hub never became ready") { hub.reload.ready? }
#
#   # Lazy error message (evaluated only on failure)
#   wait_until(timeout: 10, message: -> { "Status was #{record.status}" }) { record.done? }
#
#   # Soft version - returns boolean instead of raising
#   if wait_until?(timeout: 5) { process_stopped? }
#     # process stopped
#   else
#     # timed out, handle gracefully
#   end
#
module WaitHelper
  class TimeoutError < StandardError; end

  # Poll until block returns truthy value or timeout expires.
  #
  # @param timeout [Numeric] Maximum seconds to wait (default: 10)
  # @param poll [Numeric] Seconds between polls (default: 0.2)
  # @param message [String, Proc] Error message on timeout (optional)
  # @return [Object] The truthy value returned by the block
  # @raise [TimeoutError] If timeout expires before block returns truthy
  def wait_until(timeout: 10, poll: 0.2, message: nil)
    deadline = Time.current + timeout
    last_result = nil

    while Time.current < deadline
      last_result = yield
      return last_result if last_result
      sleep poll
    end

    msg = case message
    when Proc then message.call
    when String then message
    else "Condition not met within #{timeout}s"
    end

    raise TimeoutError, msg
  end

  # Soft version of wait_until - returns boolean instead of raising.
  #
  # @param timeout [Numeric] Maximum seconds to wait (default: 10)
  # @param poll [Numeric] Seconds between polls (default: 0.2)
  # @return [Boolean] true if condition met, false if timed out
  def wait_until?(timeout: 10, poll: 0.2)
    wait_until(timeout: timeout, poll: poll) { yield }
    true
  rescue TimeoutError
    false
  end

  # Wait for a process to stop running.
  #
  # @param pid [Integer] Process ID to check
  # @param timeout [Numeric] Maximum seconds to wait (default: 2)
  # @return [Boolean] true if process stopped, false if still running
  def wait_for_process_exit(pid, timeout: 2)
    wait_until?(timeout: timeout, poll: 0.1) { !process_running?(pid) }
  end

  private

  def process_running?(pid)
    Process.kill(0, pid)
    true
  rescue Errno::ESRCH, Errno::EPERM
    false
  end
end
