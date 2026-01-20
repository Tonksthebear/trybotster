# frozen_string_literal: true

require "application_system_test_case"

# Base class for CLI integration tests that don't require browser interactions.
#
# Extends ApplicationSystemTestCase to get the full server infrastructure
# (including Action Cable WebSocket support) but doesn't use browser assertions.
#
# Key characteristics:
# - Full server with Action Cable support (like system tests)
# - Spawns real CLI binary
# - No browser interactions - tests verify through database state
# - Faster than browser tests (no Selenium overhead for simple cases)
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
class CliIntegrationTestCase < ApplicationSystemTestCase
  include CliTestHelper

  # Use headless Chrome but we won't actually interact with the browser
  # This ensures the full server stack (including Action Cable) is running
  driven_by :selenium, using: :headless_chrome, screen_size: [ 1024, 768 ]

  setup do
    @user = users(:jason)
    @hub = create_test_hub(user: @user)
    @started_clis = []
  end

  teardown do
    # Stop all CLI processes
    @started_clis.each { |cli| stop_cli(cli) }

    # Clean up test data (non-transactional inherited from ApplicationSystemTestCase)
    @hub&.reload&.destroy rescue nil
  end

  # Override to track started CLIs for cleanup
  def start_cli(hub, **options)
    cli = super
    @started_clis << cli
    cli
  end

  # No need to override server_url - inherited from CliTestHelper
  # which uses Capybara.current_session.server
end
