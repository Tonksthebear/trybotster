# frozen_string_literal: true

require "application_system_test_case"

# E2E tests for browser-CLI terminal relay with Signal Protocol encryption.
# Tests real cryptography: libsignal on CLI, libsignal WASM in browser.
class TerminalRelayTest < ApplicationSystemTestCase
  include CliTestHelper

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
    assert connection_url.include?("#bundle="), "URL should contain PreKeyBundle"

    sign_in_as(@user)
    visit connection_url

    # Reaching "connected" proves the full crypto handshake worked:
    # X3DH key agreement, encrypted handshake, encrypted ACK
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Security banner confirms E2E
    within "[data-connection-target='securityBanner']" do
      assert_text(/E2E|encrypted/i)
    end
  end

  test "connection fails gracefully without CLI" do
    sign_in_as(@user)
    visit hub_path(@hub.identifier)

    assert_text(/connection failed|no bundle|scan qr/i, wait: 10)
  end

  test "connection restores after page refresh" do
    @cli = start_cli(@hub, timeout: 20)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    page.refresh

    # Session restored from IndexedDB
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20
  end

  test "second browser connects via shared invite link" do
    @cli = start_cli(@hub, timeout: 20)
    connection_url = @cli.connection_url

    # First browser connects normally via QR URL
    sign_in_as(@user)
    visit connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Request invite bundle via Share Hub button
    click_button "Share Hub"

    # Wait for clipboard to be populated (via shareStatus feedback)
    assert_selector "[data-connection-target='shareStatus']", text: /copied/i, wait: 10

    # Get invite URL from clipboard
    invite_url = page.evaluate_script("navigator.clipboard.readText()")
    assert invite_url.present?, "Invite URL should be in clipboard"
    assert invite_url.include?("#bundle="), "Invite URL should contain bundle fragment"
    assert invite_url != connection_url, "Invite URL should have fresh bundle"

    # Second browser (new window) uses invite
    new_window = open_new_window
    within_window new_window do
      sign_in_as(@user)
      visit invite_url
      assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20
    end

    # Both browsers should remain connected
    assert_selector "[data-connection-target='status']", text: /connected/i
    within_window new_window do
      assert_selector "[data-connection-target='status']", text: /connected/i
    end
  end

  # === Agent Interaction Tests ===

  test "agent list loads via encrypted channel" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    # Spawn agent
    message = create_and_wait_for_agent(@hub, issue_number: 999)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30

    # Agent appearing proves encrypted message flow works
    assert_selector "[data-agents-target='list'] button", text: /test-repo-999/, wait: 10
  end

  test "agent selection roundtrip works" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 888)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30

    # Select agent
    within "[data-agents-target='list']" do
      find("button", text: /test-repo-888/).click
    end

    # CLI responds with selection confirmation
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-888/, wait: 10
  end

  test "multiple agents can be switched" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)

    # Spawn two agents
    [555, 556].each { |n| create_and_wait_for_agent(@hub, issue_number: n) }

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30

    # Both visible
    assert_selector "[data-agents-target='list'] button", text: /test-repo-555/, wait: 10
    assert_selector "[data-agents-target='list'] button", text: /test-repo-556/

    # Switch between them
    find("[data-agents-target='list'] button", text: /test-repo-555/).click
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-555/, wait: 10

    find("[data-agents-target='list'] button", text: /test-repo-556/).click
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-556/, wait: 10
  end

  test "keyboard input flows through encrypted channel" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 777)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30

    # Select agent and send input
    find("[data-agents-target='list'] button", text: /test-repo-777/).click
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-777/, wait: 10

    find("[data-terminal-display-target='container']").click
    page.send_keys("echo hello")
    page.send_keys(:enter)
    sleep 1

    # Connection stays active = no crypto errors
    assert_selector "[data-connection-target='status']", text: /connected/i
  end

  test "window resize flows through encrypted channel" do
    @cli = start_cli_with_agent_support(@hub, timeout: 30)
    create_and_wait_for_agent(@hub, issue_number: 666)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 30

    find("[data-agents-target='list'] button", text: /test-repo-666/).click
    assert_selector "[data-agents-target='selectedLabel']", text: /test-repo-666/, wait: 10

    # Resize triggers encrypted resize message
    page.driver.browser.manage.window.resize_to(1200, 800)
    sleep 1

    # Connection stays active = resize message worked
    assert_selector "[data-connection-target='status']", text: /connected/i
  end

  # === Error Recovery Tests ===

  test "UI remains functional after CLI crash" do
    @cli = start_cli(@hub, timeout: 20)

    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Kill CLI
    Process.kill("KILL", @cli.pid)
    Process.wait(@cli.pid) rescue nil

    # UI shouldn't crash
    assert_selector "[data-controller~='connection']"
    assert_selector "[data-terminal-display-target='container']"
  end

  test "each CLI instance has unique keys" do
    @cli = start_cli(@hub, timeout: 20)
    first_bundle = @cli.connection_url.split("#bundle=").last

    stop_cli(@cli)
    sleep 1

    @cli = start_cli(@hub, timeout: 20)
    second_bundle = @cli.connection_url.split("#bundle=").last

    refute_equal first_bundle, second_bundle, "New CLI should have different keys"
  end

  test "stale session requires new QR scan" do
    # Connect to CLI #1
    @cli = start_cli(@hub, timeout: 20)
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Restart CLI (new keys)
    stop_cli(@cli)
    sleep 1
    @cli = start_cli(@hub, timeout: 20)

    # Visit without new bundle - cached session won't work
    visit hub_path(@hub.identifier)
    assert_text(/connection failed|no bundle|scan qr/i, wait: 15)

    # New bundle works
    visit root_path
    sleep 0.5
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 25
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
    temp_dir = Dir.mktmpdir("cli_relay_test_")
    worktree_base = Dir.mktmpdir("cli_worktrees_")
    @test_temp_dirs ||= []
    @test_temp_dirs << temp_dir
    @test_temp_dirs << worktree_base

    # Set up git repo with test .botster_init
    setup_test_git_repo(temp_dir, hub.repo)

    # Create device token
    device_token = hub.user.device_tokens.create!(name: "Relay Test #{SecureRandom.hex(4)}")

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
        ready = IO.select([stdout_r, stderr_r], nil, nil, 0.1)
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
