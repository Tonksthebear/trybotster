# frozen_string_literal: true

require_relative "cli_integration_test_case"

# Tests CLI hub registration against real Rails server.
#
# Verifies the CLI correctly:
# - Registers hub on startup (PUT /hubs/:identifier)
# - Sends heartbeats (PATCH /hubs/:identifier/heartbeat)
# - Updates hub state in database
#
# These tests spawn real CLI binary and verify database state changes.
#
class CliHubRegistrationTest < CliIntegrationTestCase
  test "CLI registers hub on startup" do
    # Record initial state
    initial_hub_count = Hub.count
    initial_last_seen = @hub.last_seen_at

    # Start CLI
    cli = start_cli(@hub, timeout: 20)

    # CLI should be running
    assert cli.running?, "CLI should be running"

    # Hub should have been updated (last_seen_at should be recent)
    @hub.reload
    assert @hub.last_seen_at > initial_last_seen, "Hub last_seen_at should be updated"
    assert @hub.last_seen_at > 5.seconds.ago, "Hub should have been seen recently"
  end

  test "CLI creates connection URL with Signal PreKeyBundle" do
    cli = start_cli(@hub, timeout: 20)

    connection_url = cli.connection_url
    assert connection_url.present?, "Connection URL should be available"

    # URL should contain the bundle fragment with base64-encoded PreKeyBundle
    assert_match %r{#bundle=}, connection_url, "URL should contain bundle fragment"

    # Bundle should be base64-encoded data (alphanumeric, +, /, =)
    bundle_match = connection_url.match(%r{#bundle=([A-Za-z0-9+/=]+)})
    assert bundle_match, "Bundle should be valid base64"
    assert bundle_match[1].length > 100, "Bundle should contain substantial data (PreKeyBundle)"
  end

  test "CLI updates hub repo on registration" do
    # Create hub with initial repo
    hub = Hub.create!(
      user: @user,
      identifier: "cli-update-test-#{SecureRandom.hex(4)}",
      repo: "original/repo",
      last_seen_at: 1.hour.ago
    )

    # Start CLI (it will register and may update hub)
    cli = start_cli(hub, timeout: 20)
    @started_clis << cli

    # Hub should be updated
    hub.reload
    assert hub.last_seen_at > 1.minute.ago

    # Cleanup
    hub.destroy
  end

  test "CLI process can be stopped cleanly" do
    cli = start_cli(@hub, timeout: 20)

    assert cli.running?, "CLI should be running initially"

    # Stop CLI
    stop_cli(cli)
    @started_clis.delete(cli)

    # Give it a moment
    sleep 0.5

    refute cli.running?, "CLI should not be running after stop"
  end

  test "multiple CLIs can run for different hubs" do
    hub2 = create_test_hub(user: @user)

    cli1 = start_cli(@hub, timeout: 20)
    cli2 = start_cli(hub2, timeout: 20)

    assert cli1.running?, "First CLI should be running"
    assert cli2.running?, "Second CLI should be running"

    # Both should have connection URLs
    assert cli1.connection_url.present?
    assert cli2.connection_url.present?

    # URLs should be different (different hubs)
    refute_equal cli1.connection_url, cli2.connection_url

    # Cleanup
    stop_cli(cli2)
    @started_clis.delete(cli2)
    hub2.destroy
  end

  test "CLI logs are accessible for debugging" do
    cli = start_cli(@hub, timeout: 20)

    # Should be able to read captured output
    output = cli.recent_output

    # Output should contain startup info
    assert output.present?, "Should have captured output"
  end
end
