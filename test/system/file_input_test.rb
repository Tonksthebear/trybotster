# frozen_string_literal: true

require "application_system_test_case"
require_relative "../support/cli_test_helper"

class FileInputTest < ApplicationSystemTestCase
  include CliTestHelper

  driven_by :selenium, using: :headless_chrome, screen_size: [ 1280, 900 ]

  setup do
    @user = users(:one)
    @hub = create_test_hub(user: @user)
    @cli_temp_dir = Dir.mktmpdir("cli_file_input_")

    # Create minimal configs before the CLI boots so config discovery sees them
    # on the first load.
    agent_init_path = File.join(@cli_temp_dir, ".botster/agents/default/initialization")
    FileUtils.mkdir_p(File.dirname(agent_init_path))
    File.write(agent_init_path, "#!/bin/bash\n")
    accessory_init_path = File.join(@cli_temp_dir, ".botster/accessories/default/initialization")
    FileUtils.mkdir_p(File.dirname(accessory_init_path))
    File.write(accessory_init_path, "#!/bin/bash\n")

    @cli = start_cli(@hub, temp_dir: @cli_temp_dir)
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

    session_uuid = create_accessory_session
    find(".terminal-container", wait: 15)
    existing_paste_files = current_paste_files

    # Inject a hidden file input and attach the test PNG via Capybara
    fixture_path = Rails.root.join("test/fixtures/files/test_image.png")
    page.execute_script("document.body.insertAdjacentHTML('beforeend', '<input type=\"file\" id=\"test-file-input\">')")
    attach_file("test-file-input", fixture_path.to_s, make_visible: true)

    # Send via TerminalConnection (not HubTransport — correct sub_id routing).
    # Capybara's .drop() doesn't work here because Chrome's synthetic DragEvent
    # doesn't populate dataTransfer.files. We use attach_file + sendFile instead.
    wait_for_terminal_connection(runtime_hub.id, session_uuid)

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
    paste_file = wait_for_new_paste_file(existing_paste_files, timeout: 10)

    # Verify file contents match the fixture
    written_bytes = File.binread(paste_file)
    assert_equal png_bytes, written_bytes,
      "Paste file content should match the test fixture PNG"
  end

  test "large file (>65KB) dropped on terminal survives SCTP transfer" do
    sign_in_and_connect
    session_uuid = create_accessory_session
    find(".terminal-container", wait: 15)
    existing_paste_files = current_paste_files

    # Use the 360KB test image — exceeds Chrome's 256KB SCTP max-message-size,
    # which forces the browser to use application-level chunking (CONTENT_FILE_CHUNK).
    fixture_path = Rails.root.join("test/fixtures/files/large_test_image.png")
    assert File.size(fixture_path) > 256 * 1024, "Fixture must exceed 256KB to test chunking"

    page.execute_script("document.body.insertAdjacentHTML('beforeend', '<input type=\"file\" id=\"test-file-input\">')")
    attach_file("test-file-input", fixture_path.to_s, make_visible: true)

    wait_for_terminal_connection(runtime_hub.id, session_uuid)

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
    paste_file = wait_for_new_paste_file(existing_paste_files, timeout: 15)

    written_bytes = File.binread(paste_file)
    assert_equal png_bytes.size, written_bytes.size,
      "Paste file size should match (expected #{png_bytes.size}, got #{written_bytes.size})"
    assert_equal png_bytes, written_bytes,
      "Paste file content should match the large test fixture byte-for-byte"
  end

  private

  def sign_in_and_connect
    super(hub: runtime_hub, prewarm_hub_page: true, retry_if_stale: true)
  end

  def runtime_hub
    @cli&.hub || @hub
  end

  # Acquires a TerminalConnection directly via HubConnectionManager, bypassing
  # the Restty WASM init path (which requires WebGPU/WebGL2 that may not be
  # available in headless Chrome on CI). Stashes the connection on
  # window._botsterTestConn for subsequent sendFile calls.
  def wait_for_terminal_connection(hub_id, session_uuid)
    key = "terminal:#{hub_id}:#{session_uuid}"
    last_status = nil
    assert wait_until?(timeout: 20, poll: 0.5) {
      status = page.driver.browser.execute_async_script(<<~JS, key, hub_id.to_s, session_uuid)
        var done = arguments[arguments.length - 1];
        var key = arguments[0];
        (async function() {
          try {
            var entry = window._botsterTestTerminal && window._botsterTestTerminal[key];
            var transport = entry && entry.transport;
            if (transport && transport.isConnected()) {
              window._botsterTestConn = transport;
              done("connected");
            } else if (transport) {
              done("waiting_for_transport_connect");
            } else {
              done("waiting_for_transport");
            }
          } catch(e) { done("error: " + e.message); }
        })();
      JS
      last_status = status
      status == "connected"
    }, "TerminalConnection for #{key} did not become ready within 20s (last status: #{last_status.inspect})"
  end

  def create_accessory_session
    wait_for_surface_ready("workspace_panel")
    find("[data-testid='new-session-button']:not([disabled])", match: :first).click
    assert_text "New Session", wait: 10

    selectable_option_text = nil
    assert wait_until?(timeout: 15, poll: 0.3) {
      target_select = find("[data-testid='spawn-target-select']", wait: 2)
      selectable_option = target_select.all("option").find { |option| option.value.present? rescue false }
      selectable_option_text = selectable_option&.text
      selectable_option_text.present?
    }, "Expected at least one admitted spawn target option"

    find("[data-testid='spawn-target-select']", wait: 5).select(selectable_option_text)
    find("[data-testid='choose-accessory']", wait: 10).click

    assert_text "New Accessory", wait: 10
    find("button", text: "terminal", wait: 15).click
    assert_selector "button[data-selected='true']", text: "terminal", wait: 10
    click_button "Create Accessory"

    assert_no_text "New Accessory", wait: 10

    session_link = nil
    assert wait_until?(timeout: 30, poll: 1) {
      session_link = all("a[href*='/sessions/']").first
      next true if session_link

      visit current_url
      session_link = all("a[href*='/sessions/']").first
      session_link.present?
    }, "Expected spawned session to appear in the workspace list."
    session_uuid = session_link[:href].split("/").last
    session_link.click
    assert_current_path "/hubs/#{runtime_hub.id}/sessions/#{session_uuid}", wait: 15
    session_uuid
  end

  def current_paste_files
    Dir.glob("/tmp/botster-paste-*.png").to_set
  end

  def wait_for_new_paste_file(existing_files, timeout:)
    paste_file = nil
    assert wait_until?(timeout: timeout, poll: 0.3) {
      matches = current_paste_files.to_a - existing_files.to_a
      next false if matches.empty?

      paste_file = matches.max_by { |path| File.mtime(path) }
      paste_file.present?
    }, "Expected CLI to write a paste file to /tmp/botster-paste-*.png.\n" \
       "CLI logs:\n#{@cli.log_contents(lines: 30)}"
    paste_file
  end
end
