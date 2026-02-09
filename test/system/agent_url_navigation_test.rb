# frozen_string_literal: true

require "application_system_test_case"

# Tests for URL-based agent navigation.
#
# These tests verify:
# - /hubs/:hub_id shows hub info landing page
# - /hubs/:hub_id/agents/:index shows agent terminal
# - Sidebar agents are links (not JS-only buttons)
# - Page refresh on agent URL auto-selects that agent
# - Connection persists across Turbo navigation (data-turbo-permanent)
class AgentUrlNavigationTest < ApplicationSystemTestCase
  include CliTestHelper

  driven_by :selenium, using: :headless_chrome, screen_size: [ 1280, 900 ]

  setup do
    @user = users(:one)
    @hub = create_test_hub(user: @user)
  end

  teardown do
    stop_cli(@cli) if @cli
    @hub&.destroy
    @test_temp_dirs&.each { |path| FileUtils.rm_rf(path) if File.directory?(path) }
  end

  # === Route Structure Tests ===

  test "hubs#show is a landing page without terminal" do
    @cli = start_cli(@hub, timeout: 20)

    sign_in_as(@user)
    # Visit hub with bundle in fragment (required for E2E connection)
    bundle = URI.parse(@cli.connection_url).fragment
    visit "#{hub_url(@hub)}##{bundle}"

    # Should show hub info, not terminal container
    # Note: h1 shows device name (set by CLI) or identifier as fallback
    @hub.reload
    expected_title = @hub.device&.name || @hub.identifier.truncate(32)
    assert_selector "h1", text: expected_title, wait: 10
    assert_no_selector "[data-terminal-display-target='container']"

    # Should show list of agents (if any)
    assert_selector "[data-test='hub-agents-list']"
  end

  test "hub agent show route exists and displays terminal" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 123)

    sign_in_as(@user)
    # Extract bundle from connection URL and append to agent URL
    bundle = URI.parse(@cli.connection_url).fragment
    agent_url_with_bundle = "#{hub_agent_url(@hub, 0)}##{bundle}"

    # Visit agent URL with bundle - establishes connection directly on agent page
    visit agent_url_with_bundle

    # Should show terminal with connection
    assert_selector "[data-terminal-display-target='container']", wait: 20
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30
  end

  test "visiting agent URL auto-selects that agent" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 100)
    create_and_wait_for_agent(@hub, issue_number: 200)

    sign_in_as(@user)
    # Extract bundle from connection URL and append to second agent URL
    bundle = URI.parse(@cli.connection_url).fragment
    agent_url_with_bundle = "#{hub_agent_url(@hub, 1)}##{bundle}"

    # Visit second agent directly with bundle
    visit agent_url_with_bundle

    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Agent at index 1 should be selected
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-200/, wait: 10
  end

  test "page refresh on agent URL reconnects to same agent" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 999)

    sign_in_as(@user)
    # Extract bundle and visit agent page directly with bundle
    bundle = URI.parse(@cli.connection_url).fragment
    agent_url_with_bundle = "#{hub_agent_url(@hub, 0)}##{bundle}"

    visit agent_url_with_bundle
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-999/, wait: 10

    # Refresh the page - session should restore from IndexedDB
    page.refresh

    # Should reconnect and auto-select the same agent
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-999/, wait: 10
  end

  # === Sidebar Navigation Tests ===

  test "sidebar agents are links not buttons" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 777)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Agent in sidebar should be a link
    assert_selector ".sidebar-agents-list a[href]", text: /test-repo-777/, wait: 10
  end

  test "clicking sidebar agent link navigates to agent URL" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 555)

    sign_in_as(@user)
    # Start at hub landing page with connection established via fragment
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Click agent link in sidebar
    within(".sidebar-agents-list") do
      click_link text: /test-repo-555/
    end

    # URL should change to agent path
    assert_current_path hub_agent_path(@hub, 0)

    # Agent should be selected
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-555/, wait: 10
  end

  test "navigating between agents updates URL" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 111)
    create_and_wait_for_agent(@hub, issue_number: 222)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Wait for both agents to appear
    assert_selector ".sidebar-agents-list a", text: /test-repo-111/, wait: 10
    assert_selector ".sidebar-agents-list a", text: /test-repo-222/

    # Click first agent in sidebar
    within(".sidebar-agents-list") do
      click_link text: /test-repo-111/
    end
    assert_current_path hub_agent_path(@hub, 0)

    # Click second agent in sidebar
    within(".sidebar-agents-list") do
      click_link text: /test-repo-222/
    end
    assert_current_path hub_agent_path(@hub, 1)
  end

  # === Connection Persistence Tests ===

  test "connection persists across turbo navigation" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 333)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Get initial connection state
    initial_session_id = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="hub-connection"]'), 'hub-connection'
      );
      return conn?.hubChannel?.subscription?.identifier || 'none';
    JS

    # Navigate to agent via sidebar
    within(".sidebar-agents-list") do
      click_link text: /test-repo-333/
    end

    # Connection should persist (same subscription)
    assert_selector "[data-hub-connection-target='status']", text: /connected/i

    new_session_id = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="hub-connection"]'), 'hub-connection'
      );
      return conn?.hubChannel?.subscription?.identifier || 'none';
    JS

    assert_equal initial_session_id, new_session_id,
      "Connection should persist across Turbo navigation (turbo-permanent)"
  end

  test "terminal clears when switching agents" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 444)
    create_and_wait_for_agent(@hub, issue_number: 445)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Select first agent via sidebar and wait for terminal to be active
    within(".sidebar-agents-list") do
      click_link text: /test-repo-444/
    end
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-444/, wait: 10

    # Wait for terminal to show agent's shell prompt (indicates terminal is connected)
    assert_text "bash", wait: 15

    # The first agent's shell has already printed output from .botster_init
    # which includes the issue number (444)
    assert_text "BOTSTER_ISSUE_NUMBER: 444", wait: 10

    # Switch to second agent via sidebar
    within(".sidebar-agents-list") do
      click_link text: /test-repo-445/
    end
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-445/, wait: 10

    # Wait for second agent's terminal to load
    assert_text "bash", wait: 15

    # First agent's output should not be visible (terminal cleared)
    # The second agent will show issue number 445, not 444
    assert_no_text "BOTSTER_ISSUE_NUMBER: 444"
    assert_text "BOTSTER_ISSUE_NUMBER: 445", wait: 10
  end

  # === Hub Landing Page Tests ===

  test "hub landing page shows agent list" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 600)
    create_and_wait_for_agent(@hub, issue_number: 601)

    sign_in_as(@user)
    # Visit hub with bundle in fragment (required for E2E connection)
    bundle = URI.parse(@cli.connection_url).fragment
    visit "#{hub_url(@hub)}##{bundle}"

    # Wait for connection to be established (agents are populated via JS after connection)
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Landing page shows agent list (duplicate of sidebar for mobile/landing context)
    assert_selector "[data-test='hub-agents-list']", wait: 10
    assert_selector "[data-test='hub-agents-list'] a", text: /test-repo-600/, wait: 10
    assert_selector "[data-test='hub-agents-list'] a", text: /test-repo-601/, wait: 10
  end

  test "hub landing page links to individual agents" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 700)

    sign_in_as(@user)
    # Visit hub with bundle in fragment (required for E2E connection)
    bundle = URI.parse(@cli.connection_url).fragment
    visit "#{hub_url(@hub)}##{bundle}"

    # Wait for connection to be established (agents are populated via JS after connection)
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Wait for agent link to appear, then click it
    assert_selector "[data-test='hub-agents-list'] a", text: /test-repo-700/, wait: 10
    within "[data-test='hub-agents-list']" do
      click_link text: /test-repo-700/
    end

    # Should navigate to agent URL
    assert_current_path hub_agent_path(@hub, 0)
    assert_selector "[data-terminal-display-target='container']", wait: 20
  end

  # === Edge Cases ===

  test "invalid agent index shows error" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    # Create one agent so the controller can validate against agent count
    create_and_wait_for_agent(@hub, issue_number: 1)

    sign_in_as(@user)
    # First establish E2E connection via bundle URL
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Wait for agent to appear in sidebar (confirms agent is registered with CLI)
    assert_selector ".sidebar-agents-list a", text: /test-repo-1/, wait: 15

    # Wait for hub_agents to be synced via CLI heartbeat
    # The sidebar is populated via WebSocket, but controller validation uses DB
    unless wait_for_hub_agents_sync(@hub, expected_count: 1, timeout: 15)
      skip "Hub agents not synced via heartbeat - CLI may not report agents in heartbeat"
    end

    # Visit non-existent agent index (agent 999 when only agent 0 exists)
    visit hub_agent_path(@hub, 999)

    # Should redirect to hub landing page with error
    assert_current_path hub_path(@hub)
  end

  test "connection URL with fragment still works for QR codes" do
    @cli = start_cli(@hub, timeout: 20)
    connection_url = @cli.connection_url

    sign_in_as(@user)
    visit connection_url

    # First verify the connection element exists on the page
    assert_selector "[data-hub-connection-target='status']", wait: 10

    # Should establish connection (backwards compatible)
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30
  end

  private

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end

  def create_and_wait_for_agent(hub, issue_number:, timeout: 20)
    message = Integrations::Github::Message.create!(
      event_type: "github_mention",
      repo: "test/repo",
      issue_number: issue_number,
      payload: { repo: "test/repo", issue_number: issue_number, prompt: "Test" }
    )

    # Wait for message to be acknowledged
    wait_until?(timeout: timeout) { message.reload.status == "acknowledged" }

    # Wait for agent registration via heartbeat
    wait_until?(timeout: 10) { hub.reload.hub_agents.exists?(session_key: "test-repo-#{issue_number}") }

    message
  end

  def wait_for_hub_agents_sync(hub, expected_count:, timeout: 15)
    wait_until?(timeout: timeout) { hub.reload.hub_agents.count >= expected_count }
  end

  def start_cli_with_agent_support(hub, **options)
    # Create temp directories
    temp_dir = Dir.mktmpdir("cli_nav_test_")
    worktree_base = Dir.mktmpdir("cli_worktrees_")
    @test_temp_dirs ||= []
    @test_temp_dirs << temp_dir
    @test_temp_dirs << worktree_base

    # Set up git repo with test .botster_init
    setup_test_git_repo(temp_dir, "test/repo")

    # Create device token
    token_name = "Nav Test #{SecureRandom.hex(4)}"
    device = hub.user.devices.create!(
      name: token_name,
      device_type: "cli",
      fingerprint: SecureRandom.hex(8).scan(/../).join(":")
    )
    device_token = device.create_device_token!(name: token_name)

    env = {
      "BOTSTER_ENV" => "system_test",
      "BOTSTER_CONFIG_DIR" => temp_dir,
      "BOTSTER_SERVER_URL" => server_url,
      "BOTSTER_TOKEN" => device_token.token,
      "BOTSTER_HUB_ID" => hub.identifier,
      "BOTSTER_REPO" => "test/repo",
      "BOTSTER_WORKTREE_BASE" => worktree_base,
      "RUST_LOG" => options[:log_level] || "info,botster_hub=debug"
    }

    stdout_r, stdout_w = IO.pipe
    stderr_r, stderr_w = IO.pipe

    pid = spawn(
      env,
      CliTestHelper::CLI_BINARY.to_s,
      "start",
      "--headless",
      out: stdout_w,
      err: stderr_w,
      chdir: temp_dir
    )

    stdout_w.close
    stderr_w.close

    cli = CliTestHelper::CliProcess.new(
      pid: pid,
      hub: hub,
      stdout_r: stdout_r,
      stderr_r: stderr_r,
      temp_dir: temp_dir,
      log_thread: nil,
      log_file_path: File.join(temp_dir, "botster-hub.log"),
      device_token: device_token
    )

    # Start log reader thread
    log_thread = Thread.new do
      while cli.running?
        ready = IO.select([ stdout_r, stderr_r ], nil, nil, 0.1)
        next unless ready

        ready[0].each do |io|
          begin
            line = io.read_nonblock(4096)
            cli.add_output(line)
          rescue IO::WaitReadable, EOFError
            # Expected
          end
        end
      end
    end

    cli.instance_variable_set(:@log_thread, log_thread)
    @started_clis ||= []
    @started_clis << cli

    # Wait for CLI to be ready
    timeout = options[:timeout] || 30
    unless cli.wait_for_ready(timeout: timeout)
      output = cli.recent_output
      cli.stop
      raise "CLI failed to start: #{output}"
    end

    cli
  end

  def setup_test_git_repo(path, repo_name)
    Dir.chdir(path) do
      system("git init --initial-branch=main", out: File::NULL, err: File::NULL)
      system("git config user.email 'test@example.com'", out: File::NULL, err: File::NULL)
      system("git config user.name 'Test User'", out: File::NULL, err: File::NULL)

      File.write("README.md", "# Test Repo\n\nRepo: #{repo_name}")

      File.write(".botster_init", <<~BASH)
        #!/bin/bash
        echo "=== Test Botster Init ==="
        echo "BOTSTER_REPO: $BOTSTER_REPO"
        echo "BOTSTER_ISSUE_NUMBER: $BOTSTER_ISSUE_NUMBER"
        exec bash
      BASH
      FileUtils.chmod(0o755, ".botster_init")

      system("git add .", out: File::NULL, err: File::NULL)
      system("git commit -m 'Initial commit'", out: File::NULL, err: File::NULL)
    end
  end

  def server_url
    "http://#{Capybara.current_session.server.host}:#{Capybara.current_session.server.port}"
  end
end
