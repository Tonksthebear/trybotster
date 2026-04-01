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
    # Create minimal agent config so CLI can spawn agents
    init_path = File.join(@cli.temp_dir, ".botster/agents/default/initialization")
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

  test "small file dropped on terminal is written to tmp by CLI" do
    sign_in_and_connect

    # Create an agent so we have a terminal to receive the file
    create_agent_via_ui

    # Navigate to agent page
    first("[data-agent-list-target='list'] a[href*='/sessions/']", wait: 15).click
    terminal_el = find("[data-controller='terminal-display']", wait: 15)
    session_uuid = terminal_el["data-terminal-display-session-uuid-value"]

    # Inject a hidden file input and attach the test PNG via Capybara
    fixture_path = Rails.root.join("test/fixtures/files/test_image.png")
    page.execute_script("document.body.insertAdjacentHTML('beforeend', '<input type=\"file\" id=\"test-file-input\">')")
    attach_file("test-file-input", fixture_path.to_s, make_visible: true)

    # Send via TerminalConnection (not HubTransport — correct sub_id routing).
    # Capybara's .drop() doesn't work here because Chrome's synthetic DragEvent
    # doesn't populate dataTransfer.files. We use attach_file + sendFile instead.
    wait_for_terminal_connection(@hub.id, session_uuid)

    result = page.driver.browser.execute_async_script(<<~JS)
      var done = arguments[arguments.length - 1];
      (async function() {
        try {
          var file = document.getElementById("test-file-input").files[0];
          if (!file) { done("error: No file attached"); return; }

          var buffer = await file.arrayBuffer();
          window._botsterTestConn.sendFile(new Uint8Array(buffer), file.name);
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

  test "large file (>65KB) dropped on terminal survives SCTP transfer" do
    sign_in_and_connect
    create_agent_via_ui

    first("[data-agent-list-target='list'] a[href*='/sessions/']", wait: 15).click
    terminal_el = find("[data-controller='terminal-display']", wait: 15)
    session_uuid = terminal_el["data-terminal-display-session-uuid-value"]

    # Use the 360KB test image — exceeds Chrome's 256KB SCTP max-message-size,
    # which forces the browser to use application-level chunking (CONTENT_FILE_CHUNK).
    fixture_path = Rails.root.join("test/fixtures/files/large_test_image.png")
    assert File.size(fixture_path) > 256 * 1024, "Fixture must exceed 256KB to test chunking"

    page.execute_script("document.body.insertAdjacentHTML('beforeend', '<input type=\"file\" id=\"test-file-input\">')")
    attach_file("test-file-input", fixture_path.to_s, make_visible: true)

    wait_for_terminal_connection(@hub.id, session_uuid)

    result = page.driver.browser.execute_async_script(<<~JS)
      var done = arguments[arguments.length - 1];
      (async function() {
        try {
          var file = document.getElementById("test-file-input").files[0];
          if (!file) { done("error: No file attached"); return; }

          var buffer = await file.arrayBuffer();
          var data = new Uint8Array(buffer);
          await window._botsterTestConn.sendFile(data, file.name);
          done("ok:" + data.length);
        } catch(e) {
          done("error: " + e.message + " | stack: " + (e.stack || "none"));
        }
      })();
    JS

    assert result&.start_with?("ok"), "sendFile should succeed for large file: #{result}"

    png_bytes = File.binread(fixture_path)
    paste_file = nil
    assert wait_until?(timeout: 15, poll: 0.3) {
      matches = Dir.glob("/tmp/botster-paste-*.png")
      if matches.any?
        paste_file = matches.first
        true
      end
    }, "Expected CLI to write large paste file to /tmp/botster-paste-*.png.\n" \
       "CLI logs:\n#{@cli.log_contents(lines: 30)}"

    written_bytes = File.binread(paste_file)
    assert_equal png_bytes.size, written_bytes.size,
      "Paste file size should match (expected #{png_bytes.size}, got #{written_bytes.size})"
    assert_equal png_bytes, written_bytes,
      "Paste file content should match the large test fixture byte-for-byte"
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

  # Polls for TerminalConnection readiness at the Ruby level, then stashes the
  # connection on window._botsterTestConn for subsequent sendFile calls.
  # This avoids a long JS polling loop inside execute_async_script which can
  # exceed Selenium's script timeout on slow CI runners.
  def wait_for_terminal_connection(hub_id, session_uuid)
    key = "terminal:#{hub_id}:#{session_uuid}"
    assert wait_until?(timeout: 20, poll: 0.5) {
      status = page.driver.browser.execute_async_script(<<~JS, key)
        var done = arguments[arguments.length - 1];
        (async function() {
          try {
            var { HubConnectionManager } = await import("connections/hub_connection_manager");
            var conn = HubConnectionManager.get(arguments[0]);
            if (conn && conn.isConnected()) {
              window._botsterTestConn = conn;
              done("connected");
            } else {
              done("waiting");
            }
          } catch(e) { done("error: " + e.message); }
        })();
      JS
      status == "connected"
    }, "TerminalConnection for #{key} did not become ready within 20s"
  end

  def create_agent_via_ui
    find("#new-agent-modal", visible: :all)
    page.execute_script("document.getElementById('new-agent-modal').showModal()")

    target_select = find("[data-new-agent-form-target='targetSelect']", wait: 10)

    # Spawn targets are loaded asynchronously from the CLI via WebRTC.
    # Wait for at least one option with a non-empty value to appear.
    selectable_option = nil
    assert wait_until?(timeout: 15, poll: 0.3) {
      selectable_option = target_select.all("option").find { |option| option.value.present? }
      selectable_option.present?
    }, "Expected at least one admitted spawn target option"

    target_select.select(selectable_option.text)
    assert_no_selector "[data-new-agent-form-target='worktreeOptions'].hidden", visible: :all, wait: 10

    find("[data-action='new-agent-form#selectMainBranch']", wait: 10).click
    find("[data-action='new-agent-form#submit']", wait: 10).click

    assert_selector "[data-agent-list-target='list'] [data-agent-id]", wait: 30
  end
end
