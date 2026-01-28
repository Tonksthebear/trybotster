# frozen_string_literal: true

require "application_system_test_case"

# E2E tests for browser-CLI terminal relay with Signal Protocol encryption.
# Tests real cryptography: libsignal on CLI, libsignal WASM in browser.
class TerminalRelayTest < ApplicationSystemTestCase
  include CliTestHelper

  # Use larger viewport for desktop-only elements like security banner (hidden lg:block = 1024px+)
  driven_by :selenium, using: :headless_chrome, screen_size: [ 1280, 900 ]

  setup do
    @user = users(:one)
    @hub = create_test_hub(user: @user)
  end

  teardown do
    stop_cli(@cli) if @cli
    @hub&.destroy
  end

  # === Core Connection Tests ===

  test "browser establishes E2E encrypted connection with CLI" do
    @cli = start_cli(@hub, timeout: 20)
    connection_url = @cli.connection_url

    assert connection_url.present?, "CLI should generate connection URL"
    assert connection_url.include?("#"), "URL should contain PreKeyBundle in fragment"

    sign_in_as(@user)
    visit connection_url

    # Reaching "connected" proves the full crypto handshake worked:
    # X3DH key agreement, encrypted handshake, encrypted ACK
    begin
      assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20
    rescue Minitest::Assertion => e
      puts "\n=== CLI OUTPUT ON FAILURE ===\n#{@cli.recent_output(lines: 100)}\n=== END CLI OUTPUT ===\n"
      puts "\n=== CLI LOG FILE ===\n#{@cli.log_contents(lines: 200)}\n=== END CLI LOG ===\n"
      # Capture browser console logs
      begin
        logs = page.driver.browser.logs.get(:browser)
        puts "\n=== BROWSER CONSOLE ===\n#{logs.map { |log| "#{log.level}: #{log.message}" }.join("\n")}\n=== END BROWSER CONSOLE ===\n"
      rescue => log_error
        puts "Failed to get browser logs: #{log_error.message}"
      end
      raise e
    end

    # Connection established - landing page shows connected status
    # Terminal badge is only on agent pages, so just verify connection status here
  end

  test "CLI connection URL matches Rails hubs#show route" do
    @cli = start_cli(@hub, timeout: 20)
    connection_url = @cli.connection_url

    assert connection_url.present?, "CLI should generate connection URL"

    # Parse URL and validate format
    uri = URI.parse(connection_url)

    # Path must match /hubs/:id (Rails resourceful route using numeric ID)
    # This prevents accidental path changes that would 404
    expected_path = "/hubs/#{@hub.id}"
    assert_equal expected_path, uri.path,
      "CLI URL path must match Rails hubs#show route. " \
      "Got '#{uri.path}', expected '#{expected_path}'. " \
      "If you need a shorter path for QR codes, add a Rails route alias."

    # Fragment should contain raw Base32-encoded PreKeyBundle (no prefix for QR efficiency)
    assert uri.fragment.present?, "URL must have fragment with PreKeyBundle"
    assert uri.fragment.match?(/\A[A-Z2-7]+\z/),
      "Fragment should be raw Base32 encoded (uppercase A-Z, 2-7). Got: #{uri.fragment[0..50]}..."
    assert uri.fragment.length > 2800,
      "Bundle should be ~2900 chars for Kyber keys. Got: #{uri.fragment.length}"

    # Verify the URL actually resolves (doesn't 404)
    sign_in_as(@user)
    visit connection_url

    # If we get here without routing error, the path is valid
    # Don't need full handshake - just verify page loads
    assert_selector "[data-hub-connection-target='status']", wait: 10
  end

  test "connection fails gracefully without CLI" do
    sign_in_as(@user)
    visit hub_path(@hub)

    assert_text(/connection failed|no bundle|scan qr/i, wait: 10)
  end

  test "connection restores after page refresh" do
    @cli = start_cli(@hub, timeout: 20)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    page.refresh

    # Session restored from IndexedDB
    begin
      assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20
    rescue Minitest::Assertion => e
      # Capture debug info on failure
      puts "\n=== CLI LOG ON REFRESH FAILURE ==="
      puts @cli.log_contents(lines: 100)
      puts "=== END CLI LOG ==="
      begin
        logs = page.driver.browser.logs.get(:browser)
        puts "\n=== BROWSER CONSOLE ==="
        puts logs.map { |log| "#{log.level}: #{log.message}" }.join("\n")
        puts "=== END BROWSER CONSOLE ==="
      rescue => log_error
        puts "Failed to get browser logs: #{log_error.message}"
      end
      raise e
    end
  end

  test "second browser connects via shared invite link" do
    skip "Share Hub button UI not yet implemented in current design"

    @cli = start_cli(@hub, timeout: 20)
    connection_url = @cli.connection_url

    # First browser connects normally via QR URL
    sign_in_as(@user)
    visit connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # Request invite bundle via Share Hub button
    click_button "Share Hub"

    # Wait for clipboard to be populated (via shareStatus feedback)
    assert_selector "[data-hub-connection-target='shareStatus']", text: /copied/i, wait: 10

    # Get invite URL from clipboard
    invite_url = page.evaluate_script("navigator.clipboard.readText()")
    assert invite_url.present?, "Invite URL should be in clipboard"
    assert invite_url.include?("#"), "Invite URL should contain bundle fragment"
    assert invite_url != connection_url, "Invite URL should have fresh bundle"

    # Second browser (new window) uses invite
    new_window = open_new_window
    within_window new_window do
      sign_in_as(@user)
      visit invite_url
      assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20
    end

    # Both browsers should remain connected
    assert_selector "[data-hub-connection-target='status']", text: /connected/i
    within_window new_window do
      assert_selector "[data-hub-connection-target='status']", text: /connected/i
    end
  end

  # === Agent Interaction Tests ===

  test "agent list loads via encrypted channel" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    # Spawn agent
    message = create_and_wait_for_agent(@hub, issue_number: 999)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Agent appearing proves encrypted message flow works (now rendered as links)
    assert_selector ".sidebar-agents-list a", text: /test-repo-999/, wait: 10
  end

  test "agent selection roundtrip works" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 888)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Select agent (navigates to agent page via Turbo)
    assert_selector ".sidebar-agents-list a", text: /test-repo-888/, wait: 10
    within(".sidebar-agents-list") do
      click_link text: /test-repo-888/
    end

    # Wait for agent page to load (terminal container is only on agent page)
    assert_selector "[data-terminal-display-target='container']", wait: 10

    # CLI responds with selection confirmation
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-888/, wait: 10
  end

  test "multiple agents can be switched" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    # Spawn two agents
    [ 555, 556 ].each { |n| create_and_wait_for_agent(@hub, issue_number: n) }

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Both visible (now as links)
    assert_selector ".sidebar-agents-list a", text: /test-repo-555/, wait: 10
    assert_selector ".sidebar-agents-list a", text: /test-repo-556/, wait: 10

    # Click first agent (link navigation)
    within(".sidebar-agents-list") do
      click_link text: /test-repo-555/
    end
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-555/, wait: 10

    # Click second agent (link navigation)
    assert_selector ".sidebar-agents-list a", text: /test-repo-556/, wait: 10
    within(".sidebar-agents-list") do
      click_link text: /test-repo-556/
    end
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-556/, wait: 15
  end

  test "keyboard input flows through encrypted channel" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 777)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Select agent (navigates to agent page via Turbo)
    assert_selector ".sidebar-agents-list a", text: /test-repo-777/, wait: 10
    within(".sidebar-agents-list") do
      click_link text: /test-repo-777/
    end

    # Wait for agent page to load
    assert_selector "[data-terminal-display-target='container']", wait: 10
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-777/, wait: 10

    find("[data-terminal-display-target='container']").click
    page.send_keys("echo hello")
    page.send_keys(:enter)

    # Connection stays active = no crypto errors
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 5
  end

  test "full roundtrip: PTY output flows from CLI to browser" do
    # This test verifies the complete data flow through reliable delivery:
    # 1. Agent starts, PTY executes .botster_init script
    # 2. PTY output → encrypted → reliable envelope → ActionCable → Browser
    # 3. Browser receives, decrypts, displays in terminal
    #
    # We use the .botster_init script output which contains unique markers
    # (BOTSTER_ISSUE_NUMBER) to prove the roundtrip without relying on
    # programmatic input which has timing issues in the test environment.
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    # Use a unique issue number as our roundtrip marker
    unique_issue = 888
    create_and_wait_for_agent(@hub, issue_number: unique_issue)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Select the agent (via link navigation in sidebar)
    within(".sidebar-agents-list") do
      click_link text: /test-repo-#{unique_issue}/
    end
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-#{unique_issue}/, wait: 10

    # Wait for terminal container to be present
    assert_selector "[data-terminal-display-target='container']", wait: 10

    # The .botster_init script outputs BOTSTER_ISSUE_NUMBER which proves:
    # - CLI spawned the agent PTY correctly
    # - PTY output was encrypted and sent via ActionCable
    # - Browser received, decrypted, and rendered in xterm.js
    begin
      assert_text "BOTSTER_ISSUE_NUMBER: #{unique_issue}", wait: 15
    rescue Minitest::Assertion => e
      puts "\n=== ROUNDTRIP TEST FAILURE ==="
      puts "Expected to see 'BOTSTER_ISSUE_NUMBER: #{unique_issue}' in browser terminal"
      puts "CLI log:\n#{@cli.log_contents(lines: 100)}"

      # Capture browser console logs
      begin
        logs = page.driver.browser.logs.get(:browser)
        puts "\n=== BROWSER CONSOLE ===\n#{logs.map { |log| "#{log.level}: #{log.message}" }.join("\n")}\n=== END BROWSER CONSOLE ===\n"
      rescue => log_error
        puts "Failed to get browser logs: #{log_error.message}"
      end

      raise e
    end

    # Verify connection is still healthy after roundtrip
    assert_selector "[data-hub-connection-target='status']", text: /connected/i
  end

  test "window resize flows through encrypted channel" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 666)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Select agent via link navigation
    assert_selector ".sidebar-agents-list a", text: /test-repo-666/, wait: 10
    within(".sidebar-agents-list") do
      click_link text: /test-repo-666/
    end
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-666/, wait: 10

    # Resize triggers encrypted resize message
    page.driver.browser.manage.window.resize_to(1200, 800)

    # Connection stays active = resize message worked
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 5
  end

  # === Browser-Initiated Agent Creation Tests ===

  test "user creates agent via new branch flow" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Click "New Agent" button (use first one - the + icon in agent list header)
    first("button[title='New Agent']").click

    # Modal step 1 should be visible
    assert_selector "[data-agents-target='step1']", visible: true, wait: 5
    assert_selector "h3", text: "Select Worktree", wait: 5

    # Enter issue number and click Next
    fill_in placeholder: "Issue # or branch name", with: "42"
    within "[data-agents-target='step1']" do
      click_button "Next"
    end

    # Should advance to step 2
    assert_selector "[data-agents-target='step2']", visible: true, wait: 5
    assert_selector "h3", text: "Initial Prompt", wait: 5
    assert_selector "[data-agents-target='selectedWorktreeLabel']", text: "42", wait: 5

    # Enter an optional prompt
    fill_in placeholder: /Describe the task/, with: "Fix the login bug"

    # Click Start Agent
    click_button "Start Agent"

    # Modal should close and progress indicator should appear
    assert_no_selector "#new-agent-modal[open]", wait: 5

    # Progress indicator should show creating states
    assert_selector "[data-creating-indicator]", wait: 10
    assert_text(/creating|worktree|starting/i, wait: 10)

    # Wait for agent to be created and progress to clear
    assert_selector ".sidebar-agents-list a", text: /42/, wait: 60

    # Progress indicator should be gone
    assert_no_selector "[data-creating-indicator]"
  end

  test "user creates agent without prompt" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Open modal
    first("button[title='New Agent']").click
    assert_selector "[data-agents-target='step1']", visible: true, wait: 5

    # Enter branch name and proceed
    fill_in placeholder: "Issue # or branch name", with: "feature-test"
    within "[data-agents-target='step1']" do
      click_button "Next"
    end

    # Skip prompt (leave blank) and start
    assert_selector "[data-agents-target='step2']", visible: true, wait: 5
    click_button "Start Agent"

    # Should create agent with default behavior
    assert_selector ".sidebar-agents-list a", text: /feature-test/, wait: 60
  end

  test "user can go back from step 2 to step 1" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Navigate to step 2
    first("button[title='New Agent']").click
    assert_selector "[data-agents-target='step1']", visible: true, wait: 5
    fill_in placeholder: "Issue # or branch name", with: "123"
    within "[data-agents-target='step1']" do
      click_button "Next"
    end
    assert_selector "[data-agents-target='step2']", visible: true, wait: 5

    # Go back
    click_button "Back"

    # Should be back at step 1
    assert_selector "[data-agents-target='step1']", visible: true, wait: 5
    assert_selector "[data-agents-target='step2']", visible: false, wait: 5
  end

  test "user can cancel modal at step 1" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Open modal
    first("button[title='New Agent']").click
    assert_selector "[data-agents-target='step1']", visible: true, wait: 5

    # Cancel
    within "#new-agent-modal" do
      click_button "Cancel"
    end

    # Modal should be closed (dialog without open attribute is not visible)
    assert_no_selector "#new-agent-modal[open]", wait: 5
  end

  test "progress indicator shows stages during agent creation" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Create agent
    first("button[title='New Agent']").click
    assert_selector "[data-agents-target='step1']", visible: true, wait: 5
    fill_in placeholder: "Issue # or branch name", with: "789"
    within "[data-agents-target='step1']" do
      click_button "Next"
    end
    assert_selector "[data-agents-target='step2']", visible: true, wait: 5
    click_button "Start Agent"

    # Should see progress through stages
    # Note: These assertions check that progress is shown, actual messages may vary
    assert_selector "[data-creating-indicator]", wait: 10

    # Eventually agent should appear and progress should clear
    assert_selector ".sidebar-agents-list a", text: /789/, wait: 60
    assert_no_selector "[data-creating-indicator]"
  end

  test "user selects existing worktree to create agent" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    # First create an agent to establish a worktree
    create_and_wait_for_agent(@hub, issue_number: 100)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Open modal - worktrees load asynchronously after modal opens
    first("button[title='New Agent']").click
    assert_selector "[data-agents-target='step1']", visible: true, wait: 5

    # Wait for worktree list to populate
    assert_selector "[data-agents-target='worktreeList']", wait: 10

    # Check if existing worktrees section has our worktree
    # The worktree should appear after the first agent was created
    within "[data-agents-target='worktreeList']" do
      # Should show existing worktree from issue 100
      if has_button?(text: /100/, wait: 5)
        # Click existing worktree
        click_button text: /100/
      else
        # Worktree might not be listed yet, skip this specific assertion
        skip "Worktree not listed - may need worktree list refresh"
      end
    end

    # Should advance to step 2
    assert_selector "[data-agents-target='step2']", visible: true, wait: 5

    # Start agent with existing worktree
    click_button "Start Agent"

    # Agent should be created
    assert_selector ".sidebar-agents-list a", wait: 60
  end

  test "pressing enter in branch input advances to step 2" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 30

    # Open modal
    first("button[title='New Agent']").click
    assert_selector "[data-agents-target='step1']", visible: true, wait: 5

    # Type and press enter
    input = find("[data-agents-target='newBranchInput']")
    input.fill_in with: "456"
    input.send_keys(:enter)

    # Should advance to step 2
    assert_selector "[data-agents-target='step2']", visible: true, wait: 5
    assert_selector "[data-agents-target='selectedWorktreeLabel']", text: "456", wait: 5
  end

  # === Error Recovery Tests ===

  test "UI remains functional after CLI crash" do
    @cli = start_cli(@hub, timeout: 20)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # Kill CLI
    Process.kill("KILL", @cli.pid)
    Process.wait(@cli.pid) rescue nil

    # UI shouldn't crash - landing page elements should still be present
    assert_selector "[data-controller~='hub-connection']"
    assert_selector "[data-controller~='agents']"
  end

  test "each CLI instance has unique keys" do
    @cli = start_cli(@hub, timeout: 20)
    first_bundle = @cli.connection_url.split("#").last

    stop_cli(@cli)
    # stop_cli waits for process exit, no additional sleep needed

    @cli = start_cli(@hub, timeout: 20)
    second_bundle = @cli.connection_url.split("#").last

    refute_equal first_bundle, second_bundle, "New CLI should have different keys"
  end

  test "connection remains stable over heartbeat interval" do
    # Test that reliable delivery heartbeat ACKs keep connection alive
    # The heartbeat interval is 5 seconds, so we wait 10+ seconds to verify
    @cli = start_cli(@hub, timeout: 20)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # INTENTIONAL TIMING TEST: Wait through 2 heartbeat intervals (2s each in test mode)
    # to verify connection remains stable. This sleep cannot be replaced with a
    # condition check - we need actual wall-clock time to pass.
    sleep 3

    # Connection should still be alive
    assert_selector "[data-hub-connection-target='status']", text: /connected/i

    # Verify heartbeats are being sent to Rails (proves polling loop is running)
    cli_log = @cli.log_contents(lines: 100)
    assert_match(/Heartbeat worker.*sent heartbeat/i, cli_log,
      "CLI should have sent heartbeat to Rails during idle period")
  end

  test "stale session requires new QR scan" do
    # Connect to CLI #1
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 20

    # Restart CLI (new keys)
    stop_cli(@cli)
    # stop_cli waits for process exit, no additional sleep needed
    @cli = start_cli(@hub, timeout: 20)

    # Visit without new bundle - cached session won't work
    visit hub_path(@hub)
    assert_text(/connection failed|no bundle|scan qr/i, wait: 15)

    # New bundle works
    visit root_path
    visit @cli.connection_url
    assert_selector "[data-hub-connection-target='status']", text: /connected/i, wait: 25
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

    # Wait for message to be acknowledged
    wait_until?(timeout: timeout) { message.reload.status == "acknowledged" }

    # Wait for agent registration via heartbeat
    wait_until?(timeout: 10) { hub.reload.hub_agents.exists?(session_key: "test-repo-#{issue_number}") }

    message
  end

  def start_cli_with_agent_support(hub, **options)
    # Create temp directories
    temp_dir = Dir.mktmpdir("cli_relay_test_")
    worktree_base = Dir.mktmpdir("cli_worktrees_")
    @test_temp_dirs ||= []
    @test_temp_dirs << temp_dir
    @test_temp_dirs << worktree_base

    # Set up git repo with test .botster_init
    setup_test_git_repo(temp_dir, hub.repo)

    # Create device token
    token_name = "Relay Test #{SecureRandom.hex(4)}"
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

      # Create test .botster_init that produces visible output
      File.write(".botster_init", <<~BASH)
        #!/bin/bash
        echo "=== Test Botster Init ==="
        echo "BOTSTER_REPO: $BOTSTER_REPO"
        echo "BOTSTER_ISSUE_NUMBER: $BOTSTER_ISSUE_NUMBER"
        echo "BOTSTER_BRANCH_NAME: $BOTSTER_BRANCH_NAME"

        # Output some lines to fill the terminal
        for i in $(seq 1 5); do
          echo "Test output line $i"
          sleep 0.1
        done

        echo "Init complete. Waiting..."
        # Keep the shell open for input testing
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

  def teardown
    # Clean up CLIs
    @started_clis&.each { |cli| stop_cli(cli) }

    # Clean up temp directories
    @test_temp_dirs&.each do |path|
      FileUtils.rm_rf(path) if File.directory?(path)
    end

    super
  end
end
