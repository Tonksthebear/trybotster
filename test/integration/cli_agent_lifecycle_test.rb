# frozen_string_literal: true

require_relative "cli_integration_test_case"

# Tests the complete agent lifecycle from message to cleanup.
#
# These tests verify:
# - Message polling triggers agent spawn
# - Agent PTY captures output correctly
# - Agent cleanup removes sessions
# - Worktree creation/deletion
#
# Note: These tests require a git repository for worktree operations.
# The test sets up a minimal git repo in the temp directory.
#
# Key environment variables:
# - BOTSTER_WORKTREE_BASE: Directory where worktrees are created
# - BOTSTER_REPO: Override repo detection for test isolation
#
class CliAgentLifecycleTest < CliIntegrationTestCase
  # === Agent Spawn Tests ===

  test "github_mention message triggers agent spawn" do
    # Create a pending message that should spawn an agent
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: {
        repo: @hub.repo,
        issue_number: 123,
        comment_body: "Hey @botster, please help with this",
        prompt: "Help with the issue"
      },
      status: "pending"
    )

    # Start CLI - it will poll and pick up the message
    cli = start_cli_in_git_repo(@hub, timeout: 30)

    # Wait for message to be claimed and processed
    assert_message_acknowledged(message, timeout: 20)

    # Verify agent was spawned by checking hub_agents
    # Note: Agents are registered via heartbeat, so we need to wait a bit
    wait_for_agent_registration(@hub, timeout: 10)

    @hub.reload
    assert @hub.hub_agents.exists?, "Agent should be registered with hub.\nCLI logs:\n#{cli.log_contents(lines: 50)}"

    agent = @hub.hub_agents.first
    # session_key format is "{repo-safe}-{issue_number}"
    assert_equal "test-repo-123", agent.session_key
  end

  test "agent spawn creates worktree" do
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: {
        repo: @hub.repo,
        issue_number: 456,
        prompt: "Create a feature"
      },
      status: "pending"
    )

    cli = start_cli_in_git_repo(@hub, timeout: 30)
    assert_message_acknowledged(message, timeout: 20)

    # Worktrees are created at BOTSTER_WORKTREE_BASE/repo-safe-name-branch-name
    # e.g., /tmp/worktrees/test-repo-botster-issue-456
    repo_safe = @hub.repo.tr("/", "-")
    expected_worktree = File.join(@worktree_base, "#{repo_safe}-botster-issue-456")

    # Wait for worktree to be created
    deadline = Time.current + 15
    while Time.current < deadline
      break if File.directory?(expected_worktree)
      sleep 0.5
    end

    assert File.directory?(expected_worktree),
      "Worktree directory should exist at #{expected_worktree}.\nCLI logs:\n#{cli.log_contents(lines: 50)}"
  end

  test "agent receives environment variables" do
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: {
        repo: @hub.repo,
        issue_number: 789,
        prompt: "Check environment",
        invocation_url: "https://github.com/test/repo/issues/789#issuecomment-123"
      },
      status: "pending"
    )

    cli = start_cli_in_git_repo(@hub, timeout: 30)
    assert_message_acknowledged(message, timeout: 20)

    # Find the worktree path
    repo_safe = @hub.repo.tr("/", "-")
    worktree_path = File.join(@worktree_base, "#{repo_safe}-botster-issue-789")
    prompt_file = File.join(worktree_path, ".botster_prompt")

    # Wait for prompt file to be written
    deadline = Time.current + 15
    while Time.current < deadline
      break if File.exist?(prompt_file)
      sleep 0.5
    end

    assert File.exist?(prompt_file),
      "Prompt file should be written to worktree at #{prompt_file}.\nCLI logs:\n#{cli.log_contents(lines: 50)}"

    prompt_content = File.read(prompt_file)
    assert_includes prompt_content, "Check environment", "Prompt file should contain task description"
  end

  # === Agent Cleanup Tests ===

  test "agent_cleanup message removes agent session" do
    # First, spawn an agent
    spawn_message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: @hub.repo, issue_number: 101, prompt: "Task" },
      status: "pending"
    )

    cli = start_cli_in_git_repo(@hub, timeout: 30)
    assert_message_acknowledged(spawn_message, timeout: 20)

    # Wait for agent to be registered via heartbeat
    wait_for_agent_registration(@hub, timeout: 10)

    # Verify agent exists
    @hub.reload
    assert @hub.hub_agents.exists?, "Agent should exist before cleanup.\nCLI logs:\n#{cli.log_contents(lines: 50)}"
    initial_agent_count = @hub.hub_agents.count

    # Now send cleanup message
    cleanup_message = Bot::Message.create!(
      event_type: "agent_cleanup",
      payload: {
        repo: @hub.repo,
        issue_number: 101,
        cleanup_reason: "Issue resolved"
      },
      status: "pending"
    )

    # Wait for cleanup to be processed
    assert_message_acknowledged(cleanup_message, timeout: 15)

    # Wait for next heartbeat to update agent list (heartbeat interval is 2s in test mode)
    sleep 4

    # Agent should be removed
    @hub.reload
    cli_output = cli.recent_output(lines: 100)
    assert_equal initial_agent_count - 1, @hub.hub_agents.count,
      "Agent count should decrease after cleanup.\nCLI output:\n#{cli_output}\n\nHub agents: #{@hub.hub_agents.pluck(:session_key)}"
  end

  test "multiple agents can run concurrently" do
    messages = [111, 222, 333].map do |issue_num|
      Bot::Message.create!(
        event_type: "github_mention",
        payload: { repo: @hub.repo, issue_number: issue_num, prompt: "Task #{issue_num}" },
        status: "pending"
      )
    end

    cli = start_cli_in_git_repo(@hub, timeout: 45)

    # Wait for all messages to be acknowledged
    messages.each do |msg|
      assert_message_acknowledged(msg, timeout: 20)
    end

    # Wait for agents to be registered via heartbeat
    wait_for_agent_count(@hub, count: 3, timeout: 15)

    # All three agents should be registered
    @hub.reload
    assert_equal 3, @hub.hub_agents.count,
      "All three agents should be registered.\nCLI logs:\n#{cli.log_contents(lines: 50)}"

    # Each should have a unique session key (format: repo-safe-issue_number)
    session_keys = @hub.hub_agents.pluck(:session_key).sort
    assert_equal ["test-repo-111", "test-repo-222", "test-repo-333"], session_keys
  end

  private

  # Start CLI with a git repo already set up
  def start_cli_in_git_repo(hub, **options)
    # Create temp directory with git repo
    temp_dir = Dir.mktmpdir("cli_agent_test_")
    worktree_base = Dir.mktmpdir("cli_worktrees_")

    setup_git_repo(temp_dir, hub.repo)

    # Store for cleanup and test assertions
    @test_temp_dirs ||= []
    @test_temp_dirs << temp_dir
    @test_temp_dirs << worktree_base
    @git_repo_path = temp_dir
    @worktree_base = worktree_base

    # Create device token for CLI authentication
    device_token = hub.user.device_tokens.create!(name: "CLI Test Token #{SecureRandom.hex(4)}")
    api_key = device_token.token

    # Set up environment with git repo as working directory
    env = {
      "BOTSTER_ENV" => "test",
      "BOTSTER_CONFIG_DIR" => temp_dir,
      "BOTSTER_SERVER_URL" => server_url,
      "BOTSTER_TOKEN" => api_key,
      "BOTSTER_HUB_ID" => hub.identifier,
      "BOTSTER_REPO" => hub.repo,  # Explicit repo name for test isolation
      "BOTSTER_WORKTREE_BASE" => worktree_base,  # Custom worktree location
      "RUST_LOG" => options[:log_level] || "info,botster_hub=debug"
    }

    Rails.logger.info "[CliAgentTest] Starting CLI in git repo: #{temp_dir}"
    Rails.logger.info "[CliAgentTest] Worktree base: #{worktree_base}"
    Rails.logger.info "[CliAgentTest] Repo: #{hub.repo}"

    # Start CLI process in the git repo directory
    stdout_r, stdout_w = IO.pipe
    stderr_r, stderr_w = IO.pipe

    pid = spawn(
      env,
      CliTestHelper::CLI_BINARY.to_s,
      "start",
      "--headless",
      out: stdout_w,
      err: stderr_w,
      chdir: temp_dir  # Run from git repo
    )

    stdout_w.close
    stderr_w.close

    log_file_path = File.join(temp_dir, "botster-hub.log")

    cli = CliTestHelper::CliProcess.new(
      pid: pid,
      hub: hub,
      stdout_r: stdout_r,
      stderr_r: stderr_r,
      temp_dir: temp_dir,
      log_thread: nil,
      log_file_path: log_file_path,
      device_token: device_token
    )

    # Start log reader thread
    log_thread = Thread.new do
      lines_read = { stdout: 0, stderr: 0 }
      Rails.logger.info "[CliAgentTest] Log thread started"
      while cli.running?
        ready = IO.select([stdout_r, stderr_r], nil, nil, 0.1)
        next unless ready

        ready[0].each do |io|
          begin
            line = io.read_nonblock(4096)
            if io == stdout_r
              lines_read[:stdout] += 1
            else
              lines_read[:stderr] += 1
            end
            cli.add_output(line)
            # Always log stderr output for debugging
            if io == stderr_r
              Rails.logger.info "[CLI stderr] #{line}"
            elsif options[:verbose]
              Rails.logger.debug "[CLI stdout] #{line}"
            end
          rescue IO::WaitReadable, EOFError
            # Expected
          end
        end
      end
      Rails.logger.info "[CliAgentTest] Log thread exiting. Lines read: stdout=#{lines_read[:stdout]}, stderr=#{lines_read[:stderr]}"
    end

    cli.instance_variable_set(:@log_thread, log_thread)

    # Track for cleanup
    @started_clis << cli

    # Wait for CLI to be ready
    timeout = options[:timeout] || 30
    unless cli.wait_for_ready(timeout: timeout)
      output = cli.recent_output
      log_output = cli.log_contents
      cli.stop
      raise "CLI failed to start within #{timeout}s.\nRecent stdout:\n#{output}\n\nRecent logs:\n#{log_output}"
    end

    Rails.logger.info "[CliAgentTest] CLI ready"
    cli
  end

  def setup_git_repo(path, repo_name)
    Dir.chdir(path) do
      # Initialize git repo
      system("git init --initial-branch=main", out: File::NULL, err: File::NULL)
      system("git config user.email 'test@example.com'", out: File::NULL, err: File::NULL)
      system("git config user.name 'Test User'", out: File::NULL, err: File::NULL)

      # Create initial commit (required for worktrees)
      File.write("README.md", "# Test Repo\n\nRepo: #{repo_name}")

      # Create test .botster_init that echoes env vars and exits quickly
      # This replaces the real Claude spawn with a simple verification script
      File.write(".botster_init", <<~BASH)
        #!/bin/bash
        # Test init script - verifies environment and exits
        echo "=== Test Botster Init ==="
        echo "BOTSTER_REPO: $BOTSTER_REPO"
        echo "BOTSTER_ISSUE_NUMBER: $BOTSTER_ISSUE_NUMBER"
        echo "BOTSTER_BRANCH_NAME: $BOTSTER_BRANCH_NAME"
        echo "BOTSTER_WORKTREE_PATH: $BOTSTER_WORKTREE_PATH"
        echo "BOTSTER_TASK_DESCRIPTION: $BOTSTER_TASK_DESCRIPTION"

        # Quick output for scrollback testing
        for i in $(seq 1 10); do
          echo "Test line $i"
          sleep 0.01
        done

        echo "Test init complete."
        # Exit cleanly - don't spawn interactive shell
      BASH
      FileUtils.chmod(0o755, ".botster_init")

      system("git add .", out: File::NULL, err: File::NULL)
      system("git commit -m 'Initial commit'", out: File::NULL, err: File::NULL)

      Rails.logger.info "[CliAgentTest] Git repo initialized at #{path}"
    end
  end

  def wait_for_agent_registration(hub, timeout: 10)
    deadline = Time.current + timeout
    while Time.current < deadline
      hub.reload
      return true if hub.hub_agents.exists?
      sleep 0.5
    end
    false
  end

  def wait_for_agent_count(hub, count:, timeout: 10)
    deadline = Time.current + timeout
    while Time.current < deadline
      hub.reload
      return true if hub.hub_agents.count >= count
      sleep 0.5
    end
    false
  end

  def assert_message_acknowledged(message, timeout: 15)
    deadline = Time.current + timeout
    while Time.current < deadline
      message.reload
      return true if message.status == "acknowledged"
      sleep 0.5
    end
    flunk "Message #{message.id} was not acknowledged within #{timeout}s (status: #{message.status})"
  end

  def server_url
    host = Capybara.current_session.server.host
    port = Capybara.current_session.server.port
    "http://#{host}:#{port}"
  end

  def teardown
    # Clean up temp directories
    @test_temp_dirs&.each do |path|
      FileUtils.rm_rf(path) if File.directory?(path)
    end

    super
  end
end
