# frozen_string_literal: true

require_relative "cli_integration_test_case"

# Tests the GitHub plugin integration: message → ActionCable → Lua plugin → agent spawn.
#
# GitHub is a PLUGIN integration, not core. These tests verify the full flow
# through the github.lua plugin loaded from .botster/shared/plugins/github/.
#
# The test sets up:
# 1. A git repo with .botster/ directory structure (sessions + github plugin)
# 2. Github::App stubs (can't call real GitHub API in tests)
# 3. User github_app_token (required for EventsChannel subscription)
# 4. Integrations::Github::Message records that flow through the plugin
#
# What's REAL (not mocked):
# - CLI binary (real Rust daemon)
# - ActionCable (real WebSocket connection to real Puma server)
# - Lua plugin loading and execution
# - Agent spawn, worktree creation, PTY sessions
# - Message acknowledgment flow
#
# What's STUBBED (unavoidable):
# - Github::App.get_installation_for_repo (requires real GitHub App)
# - Github::App.create_issue_reaction / create_comment_reaction (GitHub API)
#
class CliGithubIntegrationTest < CliIntegrationTestCase
  include GithubTestHelper

  TEST_REPO = "test/repo"

  setup do
    @test_repo = TEST_REPO
    # Set user's github_app_token so EventsChannel accepts the subscription
    @user.update!(
      github_app_token: "ghu_test_token_for_integration",
      github_app_token_expires_at: 1.day.from_now
    )
  end

  # === Agent Spawn via GitHub Plugin ===

  test "github_mention message triggers agent spawn" do
    with_stubbed_github do
      message = create_github_message(issue_number: 123, prompt: "Help with the issue")

      cli = start_cli_in_git_repo(@hub, timeout: 30)

      assert_message_acknowledged(message, timeout: 20)

      wait_for_agent_registration(@hub, timeout: 10)

      @hub.reload
      assert @hub.hub_agents.exists?,
        "Agent should be registered with hub.\nCLI logs:\n#{cli.log_contents(lines: 50)}"

      agent = @hub.hub_agents.first
      assert_equal "test-repo-123", agent.session_key
    end
  end

  test "agent spawn creates worktree" do
    with_stubbed_github do
      message = create_github_message(issue_number: 456, prompt: "Create a feature")

      cli = start_cli_in_git_repo(@hub, timeout: 30)
      assert_message_acknowledged(message, timeout: 20)

      repo_safe = @test_repo.tr("/", "-")
      expected_worktree = File.join(@worktree_base, "#{repo_safe}-botster-issue-456")

      wait_until(
        timeout: 15,
        message: -> { "Worktree directory should exist at #{expected_worktree}.\nCLI logs:\n#{cli.log_contents(lines: 50)}" }
      ) { File.directory?(expected_worktree) }
    end
  end

  test "agent receives prompt file in worktree" do
    with_stubbed_github do
      message = create_github_message(
        issue_number: 789,
        prompt: "Check environment",
        extra_payload: { invocation_url: "https://github.com/test/repo/issues/789#issuecomment-123" }
      )

      cli = start_cli_in_git_repo(@hub, timeout: 30)
      assert_message_acknowledged(message, timeout: 20)

      repo_safe = @test_repo.tr("/", "-")
      worktree_path = File.join(@worktree_base, "#{repo_safe}-botster-issue-789")
      prompt_file = File.join(worktree_path, ".botster_prompt")

      wait_until(
        timeout: 15,
        message: -> { "Prompt file should be written at #{prompt_file}.\nCLI logs:\n#{cli.log_contents(lines: 50)}" }
      ) { File.exist?(prompt_file) }

      prompt_content = File.read(prompt_file)
      assert_includes prompt_content, "Check environment", "Prompt file should contain task description"
    end
  end

  # === Agent Cleanup via GitHub Plugin ===

  test "agent_cleanup message removes agent session" do
    with_stubbed_github do
      spawn_message = create_github_message(issue_number: 101, prompt: "Task")

      cli = start_cli_in_git_repo(@hub, timeout: 30)
      assert_message_acknowledged(spawn_message, timeout: 20)

      wait_for_agent_registration(@hub, timeout: 10)

      @hub.reload
      assert @hub.hub_agents.exists?,
        "Agent should exist before cleanup.\nCLI logs:\n#{cli.log_contents(lines: 50)}"
      initial_agent_count = @hub.hub_agents.count

      cleanup_message = Integrations::Github::Message.create!(
        event_type: "agent_cleanup",
        repo: @test_repo,
        issue_number: 101,
        payload: {
          repo: @test_repo,
          issue_number: 101,
          cleanup_reason: "Issue resolved"
        }
      )

      assert_message_acknowledged(cleanup_message, timeout: 20)

      expected_count = initial_agent_count - 1
      wait_until(
        timeout: 10,
        message: -> {
          "Agent count should decrease after cleanup.\nHub agents: #{@hub.reload.hub_agents.pluck(:session_key)}"
        }
      ) { @hub.reload.hub_agents.count == expected_count }
    end
  end

  test "multiple agents can run concurrently" do
    with_stubbed_github do
      messages = [ 111, 222, 333 ].map do |issue_num|
        create_github_message(issue_number: issue_num, prompt: "Task #{issue_num}")
      end

      cli = start_cli_in_git_repo(@hub, timeout: 45)

      messages.each do |msg|
        assert_message_acknowledged(msg, timeout: 20)
      end

      wait_for_agent_count(@hub, count: 3, timeout: 15)

      @hub.reload
      assert_equal 3, @hub.hub_agents.count,
        "All three agents should be registered.\nCLI logs:\n#{cli.log_contents(lines: 50)}"

      session_keys = @hub.hub_agents.pluck(:session_key).sort
      assert_equal [ "test-repo-111", "test-repo-222", "test-repo-333" ], session_keys
    end
  end

  private

  # Create a github_mention message with standard fields
  def create_github_message(issue_number:, prompt:, extra_payload: {})
    Integrations::Github::Message.create!(
      event_type: "github_mention",
      repo: @test_repo,
      issue_number: issue_number,
      payload: {
        repo: @test_repo,
        issue_number: issue_number,
        prompt: prompt
      }.merge(extra_payload)
    )
  end

  # Start CLI with a git repo and .botster/ config directory set up.
  # Installs the github.lua plugin and agent session config so the
  # full GitHub integration flow works.
  def start_cli_in_git_repo(hub, **options)
    build_cli unless skip_build?

    temp_dir = Dir.mktmpdir("cli_github_test_")
    worktree_base = Dir.mktmpdir("cli_worktrees_")

    setup_git_repo(temp_dir, TEST_REPO)
    install_github_plugin(temp_dir)
    install_agent_session(temp_dir)

    @test_temp_dirs ||= []
    @test_temp_dirs << temp_dir
    @test_temp_dirs << worktree_base
    @git_repo_path = temp_dir
    @worktree_base = worktree_base

    # Create device token for CLI authentication
    token_name = "CLI Test Token #{SecureRandom.hex(4)}"
    device = hub.user.devices.create!(
      name: token_name,
      device_type: "cli",
      fingerprint: SecureRandom.hex(8).scan(/../).join(":")
    )
    device_token = device.create_device_token!(name: token_name)
    api_key = device_token.token

    env = {
      "BOTSTER_ENV" => "system_test",
      "BOTSTER_CONFIG_DIR" => temp_dir,
      "BOTSTER_SERVER_URL" => server_url,
      "BOTSTER_TOKEN" => api_key,
      "BOTSTER_HUB_ID" => hub.identifier,
      "BOTSTER_REPO" => TEST_REPO,
      "BOTSTER_WORKTREE_BASE" => worktree_base,
      "RUST_LOG" => options[:log_level] || "info,botster=debug"
    }

    Rails.logger.info "[CliGithubTest] Starting CLI in git repo: #{temp_dir}"
    Rails.logger.info "[CliGithubTest] Worktree base: #{worktree_base}"

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

    log_file_path = File.join(temp_dir, "botster.log")

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

    log_thread = Thread.new do
      while cli.running?
        ready = IO.select([ stdout_r, stderr_r ], nil, nil, 0.1)
        next unless ready

        ready[0].each do |io|
          begin
            line = io.read_nonblock(4096)
            cli.add_output(line)
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
    end

    cli.instance_variable_set(:@log_thread, log_thread)
    @started_clis << cli

    timeout = options[:timeout] || 30
    unless cli.wait_for_ready(timeout: timeout)
      output = cli.recent_output
      log_output = cli.log_contents
      cli.stop
      raise "CLI failed to start within #{timeout}s.\nRecent stdout:\n#{output}\n\nRecent logs:\n#{log_output}"
    end

    Rails.logger.info "[CliGithubTest] CLI ready"
    cli
  end

  def setup_git_repo(path, repo_name)
    Dir.chdir(path) do
      system("git init --initial-branch=main", out: File::NULL, err: File::NULL)
      system("git config user.email 'test@example.com'", out: File::NULL, err: File::NULL)
      system("git config user.name 'Test User'", out: File::NULL, err: File::NULL)

      File.write("README.md", "# Test Repo\n\nRepo: #{repo_name}")

      system("git add .", out: File::NULL, err: File::NULL)
      system("git commit -m 'Initial commit'", out: File::NULL, err: File::NULL)

      Rails.logger.info "[CliGithubTest] Git repo initialized at #{path}"
    end
  end

  # Install the github.lua plugin into the repo's .botster/ directory.
  # The Lua runtime discovers plugins via ConfigResolver scanning
  # {repo}/.botster/shared/plugins/{name}/init.lua
  def install_github_plugin(repo_path)
    plugin_dir = File.join(repo_path, ".botster", "shared", "plugins", "github")
    FileUtils.mkdir_p(plugin_dir)

    source = Rails.root.join("app/templates/plugins/github.lua")
    dest = File.join(plugin_dir, "init.lua")

    # Copy plugin, stripping template metadata comments (@template, @dest, etc.)
    content = File.read(source)
    cleaned = content.lines.reject { |l| l.match?(/^-- @(template|description|category|dest|scope|version)\b/) }.join
    File.write(dest, cleaned)

    # Commit the plugin so worktrees inherit it
    Dir.chdir(repo_path) do
      system("git add .botster/", out: File::NULL, err: File::NULL)
      system("git commit -m 'Add github plugin'", out: File::NULL, err: File::NULL)
    end

    Rails.logger.info "[CliGithubTest] Installed github plugin at #{dest}"
  end

  # Install a minimal agent session config so ConfigResolver.resolve_all()
  # finds the required 'agent' session. The initialization script is a
  # simple test script that verifies environment and exits.
  def install_agent_session(repo_path)
    session_dir = File.join(repo_path, ".botster", "shared", "sessions", "agent")
    FileUtils.mkdir_p(session_dir)

    File.write(File.join(session_dir, "initialization"), <<~BASH)
      #!/bin/bash
      # Test init script - verifies environment and exits
      echo "=== Test Botster Init ==="
      echo "BOTSTER_REPO: $BOTSTER_REPO"
      echo "BOTSTER_ISSUE_NUMBER: $BOTSTER_ISSUE_NUMBER"
      echo "BOTSTER_BRANCH_NAME: $BOTSTER_BRANCH_NAME"
      echo "BOTSTER_WORKTREE_PATH: $BOTSTER_WORKTREE_PATH"
      echo "BOTSTER_TASK_DESCRIPTION: $BOTSTER_TASK_DESCRIPTION"

      for i in $(seq 1 10); do
        echo "Test line $i"
        sleep 0.01
      done

      echo "Test init complete."
    BASH
    FileUtils.chmod(0o755, File.join(session_dir, "initialization"))

    # Commit so worktrees inherit the session config
    Dir.chdir(repo_path) do
      system("git add .botster/", out: File::NULL, err: File::NULL)
      system("git commit -m 'Add agent session config'", out: File::NULL, err: File::NULL)
    end

    Rails.logger.info "[CliGithubTest] Installed agent session config"
  end

  def wait_for_agent_registration(hub, timeout: 10)
    wait_until?(timeout: timeout) { hub.reload.hub_agents.exists? }
  end

  def wait_for_agent_count(hub, count:, timeout: 10)
    wait_until?(timeout: timeout) { hub.reload.hub_agents.count >= count }
  end

  def assert_message_acknowledged(message, timeout: 15)
    wait_until(
      timeout: timeout,
      message: -> {
        cli_log = @git_repo_path && File.exist?(File.join(@git_repo_path, "botster.log")) ?
          File.read(File.join(@git_repo_path, "botster.log")).lines.last(80).join : "no log file"
        cli_output = @started_clis&.first&.recent_output || "no output"
        "Message #{message.id} not acked within #{timeout}s (status: #{message.reload.status})\n\nCLI LOG:\n#{cli_log}\n\nCLI OUTPUT:\n#{cli_output}"
      }
    ) { message.reload.status == "acknowledged" }
  end

  def teardown
    @test_temp_dirs&.each do |path|
      FileUtils.rm_rf(path) if File.directory?(path)
    end

    # Clean up github messages created during tests
    Integrations::Github::Message.where(repo: TEST_REPO).delete_all

    super
  end
end
