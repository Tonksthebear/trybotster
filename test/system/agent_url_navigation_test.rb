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
    # Visit hub without fragment - should show landing page
    visit hub_path(@hub)

    # Should show hub info, not terminal container
    assert_selector "h1", text: @hub.identifier.truncate(32), wait: 10
    assert_no_selector "[data-terminal-display-target='container']"

    # Should show list of agents (if any)
    assert_selector "[data-test='hub-agents-list']"
  end

  test "hub agent show route exists and displays terminal" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 123)

    sign_in_as(@user)
    # Visit agent URL directly
    visit hub_agent_path(@hub, 0)

    # Should show terminal
    assert_selector "[data-terminal-display-target='container']", wait: 20
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20
  end

  test "visiting agent URL auto-selects that agent" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 100)
    create_and_wait_for_agent(@hub, issue_number: 200)

    sign_in_as(@user)
    # Visit second agent directly
    visit hub_agent_path(@hub, 1)

    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30

    # Agent at index 1 should be selected
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-200/, wait: 10
  end

  test "page refresh on agent URL reconnects to same agent" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 999)

    sign_in_as(@user)
    visit hub_agent_path(@hub, 0)
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-999/, wait: 10

    # Refresh the page
    page.refresh

    # Should reconnect and auto-select the same agent
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-999/, wait: 10
  end

  # === Sidebar Navigation Tests ===

  test "sidebar agents are links not buttons" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 777)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30

    # Agent in sidebar should be a link
    assert_selector ".sidebar-agents-list a[href]", text: /test-repo-777/, wait: 10
  end

  test "clicking sidebar agent link navigates to agent URL" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 555)

    sign_in_as(@user)
    # Start at hub landing page with connection established via fragment
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30

    # Click agent link
    click_link text: /test-repo-555/

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
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30

    # Wait for both agents to appear
    assert_selector ".sidebar-agents-list a", text: /test-repo-111/, wait: 10
    assert_selector ".sidebar-agents-list a", text: /test-repo-222/

    # Click first agent
    click_link text: /test-repo-111/
    assert_current_path hub_agent_path(@hub, 0)

    # Click second agent
    click_link text: /test-repo-222/
    assert_current_path hub_agent_path(@hub, 1)
  end

  # === Connection Persistence Tests ===

  test "connection persists across turbo navigation" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 333)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30

    # Get initial connection state
    initial_session_id = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
      );
      return conn?.hubChannel?.subscription?.identifier || 'none';
    JS

    # Navigate to agent
    click_link text: /test-repo-333/

    # Connection should persist (same subscription)
    assert_selector "[data-connection-target='status']", text: /connected/i

    new_session_id = page.execute_script(<<~JS)
      const conn = window.Stimulus.getControllerForElementAndIdentifier(
        document.querySelector('[data-controller~="connection"]'), 'connection'
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
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30

    # Select first agent and send some output
    click_link text: /test-repo-444/
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-444/, wait: 10

    unique_marker = "AGENT444_#{SecureRandom.hex(4)}"
    page.execute_script(<<~JS)
      const controller = document.querySelector('[data-controller~="connection"]');
      const connectionController = window.Stimulus.getControllerForElementAndIdentifier(controller, 'connection');
      connectionController.sendInput("echo #{unique_marker}\\r");
    JS
    assert_text unique_marker, wait: 10

    # Switch to second agent
    click_link text: /test-repo-445/
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-445/, wait: 10

    # First agent's output should not be visible (terminal cleared)
    assert_no_text unique_marker
  end

  # === Hub Landing Page Tests ===

  test "hub landing page shows agent list" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 600)
    create_and_wait_for_agent(@hub, issue_number: 601)

    sign_in_as(@user)
    visit hub_path(@hub)

    # Landing page shows agent list (duplicate of sidebar for mobile/landing context)
    assert_selector "[data-test='hub-agents-list']", wait: 10
    assert_selector "[data-test='hub-agents-list'] a", text: /test-repo-600/
    assert_selector "[data-test='hub-agents-list'] a", text: /test-repo-601/
  end

  test "hub landing page links to individual agents" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 700)

    sign_in_as(@user)
    visit hub_path(@hub)

    # Click agent from landing page
    within "[data-test='hub-agents-list']" do
      click_link text: /test-repo-700/
    end

    # Should navigate to agent URL
    assert_current_path hub_agent_path(@hub, 0)
    assert_selector "[data-terminal-display-target='container']", wait: 20
  end

  # === Edge Cases ===

  test "invalid agent index shows error" do
    @cli = start_cli(@hub, timeout: 20)

    sign_in_as(@user)
    # Visit non-existent agent index
    visit hub_agent_path(@hub, 999)

    # Should show error or redirect to hub landing
    assert_text(/agent not found|no agent/i, wait: 10).or(
      assert_current_path(hub_path(@hub))
    )
  end

  test "connection URL with fragment still works for QR codes" do
    @cli = start_cli(@hub, timeout: 20)
    connection_url = @cli.connection_url

    sign_in_as(@user)
    visit connection_url

    # Should establish connection (backwards compatible)
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20
  end

  private

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end

  def create_and_wait_for_agent(hub, issue_number:, timeout: 20)
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: hub.repo, issue_number: issue_number, prompt: "Test" },
      status: "pending"
    )

    deadline = Time.current + timeout
    while Time.current < deadline
      message.reload
      break if message.status == "acknowledged"
      sleep 0.5
    end

    # Wait for agent registration via heartbeat
    deadline = Time.current + 10
    while Time.current < deadline
      hub.reload
      break if hub.hub_agents.exists?(session_key: "test-repo-#{issue_number}")
      sleep 0.5
    end

    message
  end

  def start_cli_with_agent_support(hub, **options)
    # Create temp directories
    temp_dir = Dir.mktmpdir("cli_nav_test_")
    worktree_base = Dir.mktmpdir("cli_worktrees_")
    @test_temp_dirs ||= []
    @test_temp_dirs << temp_dir
    @test_temp_dirs << worktree_base

    # Set up git repo with test .botster_init
    setup_test_git_repo(temp_dir, hub.repo)

    # Create device token
    token_name = "Nav Test #{SecureRandom.hex(4)}"
    device = hub.user.devices.create!(
      name: token_name,
      device_type: "cli",
      fingerprint: SecureRandom.hex(8).scan(/../).join(":")
    )
    device_token = device.create_device_token!(name: token_name)

    env = {
      "BOTSTER_ENV" => "test",
      "BOTSTER_CONFIG_DIR" => temp_dir,
      "BOTSTER_SERVER_URL" => server_url,
      "BOTSTER_TOKEN" => device_token.token,
      "BOTSTER_HUB_ID" => hub.identifier,
      "BOTSTER_REPO" => hub.repo,
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
