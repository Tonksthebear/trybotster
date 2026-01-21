# frozen_string_literal: true

require "test_helper"
require "capybara"

# Base class for CLI integration tests.
#
# These are proper Rails integration tests that spawn a real HTTP server
# for the CLI binary to connect to. Unlike system tests, they don't use
# a browser - all assertions are through database state and CLI output.
#
# Key characteristics:
# - Real Puma server via Capybara (no browser/Selenium overhead)
# - Spawns real CLI binary
# - Non-transactional (CLI process needs to see committed data)
# - Verifies through database state, not UI
#
# Usage:
#   class HubRegistrationCliTest < CliIntegrationTestCase
#     test "CLI registers hub on startup" do
#       cli = start_cli(@hub)
#       assert cli.wait_for_ready
#       @hub.reload
#       assert @hub.last_seen_at > 1.minute.ago
#     end
#   end
#
class CliIntegrationTestCase < ActionDispatch::IntegrationTest
  include CliTestHelper

  # Disable transactional tests - CLI process needs to see committed data
  self.use_transactional_tests = false

  setup do
    boot_server
    @user = users(:jason)
    @hub = create_test_hub(user: @user)
    @started_clis = []
  end

  teardown do
    # Stop all CLI processes
    @started_clis.each { |cli| stop_cli(cli) }

    # Clean up test data (non-transactional, so manual cleanup needed)
    @hub&.reload&.destroy rescue nil
  end

  # Override to track started CLIs for cleanup
  def start_cli(hub, **options)
    cli = super
    @started_clis << cli
    cli
  end

  private

  # Boot a real Puma server for CLI to connect to.
  # Uses Capybara's server infrastructure without browser overhead.
  def boot_server
    return if @server

    Capybara.server = :puma, { Silent: true }
    @server = Capybara::Server.new(Rails.application).boot
  end

  # Override CliTestHelper's server_url to use our booted server
  def server_url
    "http://#{@server.host}:#{@server.port}"
  end
end
