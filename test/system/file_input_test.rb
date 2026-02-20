# frozen_string_literal: true

require "application_system_test_case"
require_relative "../support/cli_test_helper"

class FileInputTest < ApplicationSystemTestCase
  include CliTestHelper

  driven_by :selenium, using: :headless_chrome, screen_size: [ 1280, 900 ]

  setup do
    @user = users(:one)
    @hub = create_test_hub(user: @user)
    @cli = start_cli(@hub)
    # Create minimal agent session config so CLI can spawn agents
    init_path = File.join(@cli.temp_dir, ".botster/shared/sessions/agent/initialization")
    FileUtils.mkdir_p(File.dirname(init_path))
    File.write(init_path, "#!/bin/bash\n")
    # Clean up any leftover paste files from previous test runs
    Dir.glob("/tmp/botster-paste-*").each { |f| File.delete(f) rescue nil }
  end

  teardown do
    stop_cli(@cli) if @cli
    @hub&.destroy
    # Clean up paste files created during this test
    Dir.glob("/tmp/botster-paste-*").each { |f| File.delete(f) rescue nil }
  end

  test "file dropped on terminal is written to tmp by CLI" do
    sign_in_and_connect

    # Create an agent so we have a terminal to receive the file
    create_agent_via_ui

    # Navigate to agent page — waits for terminal to connect
    first("[data-agent-list-target='list'] a[href*='/agents/']", wait: 15).click
    find("[data-controller='terminal-display']", wait: 15)

    # Inject a hidden file input and attach the test PNG via Capybara
    fixture_path = Rails.root.join("test/fixtures/files/test_image.png")
    page.execute_script("document.body.insertAdjacentHTML('beforeend', '<input type=\"file\" id=\"test-file-input\">')")
    attach_file("test-file-input", fixture_path.to_s, make_visible: true)

    # Send the file via the TerminalConnection (correctly routes with sub_id terminal_0_0)
    result = page.driver.browser.execute_async_script(<<~JS, @hub.id.to_s)
      var done = arguments[arguments.length - 1];
      var hubId = arguments[0];

      (async function() {
        try {
          var { ConnectionManager } = await import("connections/connection_manager");

          var key = "terminal:" + hubId + ":0:0";

          // Wait for the TerminalConnection to be created by terminal_display_controller
          var conn = null;
          for (var i = 0; i < 50; i++) {
            conn = ConnectionManager.get(key);
            if (conn && conn.isConnected()) break;
            conn = null;
            await new Promise(r => setTimeout(r, 200));
          }
          if (!conn) {
            done("error: TerminalConnection not ready after 10s");
            return;
          }

          var input = document.getElementById("test-file-input");
          var file = input.files[0];
          if (!file) { done("error: No file attached"); return; }

          var buffer = await file.arrayBuffer();
          conn.sendFile(new Uint8Array(buffer), file.name);
          done("ok");
        } catch(e) {
          done("error: " + e.message);
        }
      })();
    JS

    assert_equal "ok", result, "sendFile should succeed: #{result}"

    # Wait for the CLI to write the file to /tmp
    png_bytes = File.binread(fixture_path)
    paste_file = nil
    assert wait_until?(timeout: 10, poll: 0.3) {
      matches = Dir.glob("/tmp/botster-paste-*.png")
      if matches.any?
        paste_file = matches.first
        true
      end
    }, "Expected CLI to write a paste file to /tmp/botster-paste-*.png.\n" \
       "CLI logs:\n#{@cli.log_contents(lines: 30)}"

    # Verify file contents match the fixture
    written_bytes = File.binread(paste_file)
    assert_equal png_bytes, written_bytes,
      "Paste file content should match the test fixture PNG"
  end

  private

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end

  def sign_in_and_connect
    url = @cli.connection_url
    assert url.present?, "CLI should produce a connection URL"

    sign_in_as(@user)
    visit url

    # Pairing page: wait for bundle to be parsed, then click pair button
    assert_selector "[data-pairing-target='ready']", wait: 15
    find("[data-action='pairing#pair']").click

    # Wait for redirect to hub page after successful pairing
    assert_selector "[data-connection-status-target='connectionSection']", wait: 15

    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='direct'], " \
      "[data-connection-status-target='connectionSection'][data-state='relay']",
      wait: 30
    )
  end

  def create_agent_via_ui
    # Open the new agent dialog
    find("#new-agent-modal", visible: :all)
    page.execute_script("document.getElementById('new-agent-modal').showModal()")

    # Step 1: Select main branch
    find("[data-action='new-agent-form#selectMainBranch']", wait: 10).click

    # Step 2: Start agent (no prompt needed)
    find("[data-action='new-agent-form#submit']", wait: 10).click

    # Wait for agent to appear in the list
    assert_selector "[data-agent-list-target='list'] [data-agent-id]", wait: 30
  end
end
