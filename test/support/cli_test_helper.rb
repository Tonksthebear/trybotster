# frozen_string_literal: true

require_relative "wait_helper"

# Helper module for spawning and managing CLI instances in system tests.
#
# Usage in system tests:
#
#   class TerminalRelaySystemTest < ApplicationSystemTestCase
#     include CliTestHelper
#
#     setup do
#       @hub = create_test_hub
#       @cli = start_cli(@hub)
#     end
#
#     teardown do
#       stop_cli(@cli)
#     end
#
#     test "browser connects to CLI" do
#       # Get the connection URL (includes Signal PreKeyBundle)
#       url = @cli.connection_url
#       visit url
#       # ... test connection flow
#     end
#   end
#
module CliTestHelper
  include WaitHelper

  CLI_PATH = Rails.root.join("cli").freeze
  CLI_BINARY = CLI_PATH.join("target/debug/botster").freeze

  # Represents a running CLI instance
  class CliProcess
    include WaitHelper

    attr_reader :pid, :hub, :temp_dir, :log_file_path, :started_at
    attr_accessor :device_token

    def initialize(pid:, hub:, stdout_r:, stderr_r:, temp_dir:, log_thread:, log_file_path: nil, device_token: nil, started_at: nil)
      @pid = pid
      @hub = hub
      @stdout_r = stdout_r
      @stderr_r = stderr_r
      @temp_dir = temp_dir
      @log_thread = log_thread
      @log_file_path = log_file_path
      @device_token = device_token
      @started_at = started_at || Time.current
      @output_buffer = []
      @mutex = Mutex.new
    end

    def running?
      return false unless @pid

      Process.kill(0, @pid)
      true
    rescue Errno::ESRCH, Errno::EPERM
      false
    end

    def stop
      return unless @pid

      begin
        Process.kill("TERM", @pid)
        # Wait for graceful shutdown, then force kill if needed
        unless wait_for_process_exit(@pid, timeout: 1)
          Process.kill("KILL", @pid)
        end
        Process.wait(@pid)
      rescue Errno::ESRCH, Errno::ECHILD
        # Already stopped
      end

      @log_thread&.kill
      @stdout_r&.close
      @stderr_r&.close
      FileUtils.rm_rf(@temp_dir) if @temp_dir && File.directory?(@temp_dir)

      # Clean up the device token (important for non-transactional tests)
      @device_token&.destroy
    end

    # Wait for CLI to be ready (connected to relay)
    def wait_for_ready(timeout: 15)
      wait_until?(timeout: timeout, poll: 0.2) { ready? }
    end

    def ready?
      # CLI is ready when it has sent at least one heartbeat
      # Heartbeat updates hub.last_seen_at - more reliable than file-based detection
      @hub.reload
      @hub.last_seen_at.present? && @hub.last_seen_at > @started_at
    rescue ActiveRecord::RecordNotFound
      false
    end

    # Get the connection URL from the running CLI
    #
    # This URL includes the Signal PreKeyBundle in the fragment
    # and can be used to visit the hub page with encryption ready
    # Get the connection URL from the running CLI.
    # Polls for up to `timeout` seconds since bundle generation is deferred
    # and may not be complete immediately after heartbeat readiness.
    def connection_url(timeout: 15)
      return @cached_url if @cached_url

      url_path = File.join(@temp_dir, "hubs", @hub.identifier, "connection_url.txt")

      wait_until?(timeout: timeout, poll: 0.3) do
        if File.exist?(url_path)
          content = File.read(url_path).strip
          if content.present?
            @cached_url = content
            true
          end
        end
      end

      @cached_url
    end

    def add_output(line)
      @mutex.synchronize { @output_buffer << line }
    end

    def recent_output(lines: 50)
      @mutex.synchronize { @output_buffer.last(lines).join("\n") }
    end

    # Read the CLI's log file (if available)
    def log_contents(lines: 100)
      return "No log file" unless @log_file_path && File.exist?(@log_file_path)

      File.readlines(@log_file_path).last(lines).join
    rescue => e
      "Failed to read log: #{e.message}"
    end
  end

  # Build the CLI if needed
  def build_cli
    return if File.exist?(CLI_BINARY) && cli_binary_current?

    Rails.logger.info "[CliTestHelper] Building CLI..."
    Dir.chdir(CLI_PATH) do
      unless system("cargo build 2>&1")
        raise "Failed to build CLI"
      end
    end
    Rails.logger.info "[CliTestHelper] CLI build complete"
  end

  # Start a CLI instance for the given hub
  #
  # @param hub [Hub] The hub to connect to
  # @param options [Hash] Additional options
  # @option options [Integer] :timeout Startup timeout in seconds (default: 15)
  # @option options [String] :log_level Rust log level (default: "info")
  # @return [CliProcess] The running CLI process
  def start_cli(hub, **options)
    build_cli unless skip_build?

    # Create temp directory for CLI data
    temp_dir = Dir.mktmpdir("cli_test_")

    # Create device token for CLI authentication
    device_token = create_device_token_for_hub(hub)
    api_key = device_token.token
    Rails.logger.info "[CliTestHelper] Created DeviceToken id=#{device_token.id} token=#{api_key[0..15]}..."
    Rails.logger.info "[CliTestHelper] DeviceToken user_id=#{device_token.user&.id}"

    # Set up environment
    # BOTSTER_ENV=system_test enables test behaviors while making network calls:
    # - Use file storage instead of OS keyring
    # - Use test directories
    # - Full auth with test server (not skipped like BOTSTER_ENV=test)
    env = {
      "BOTSTER_ENV" => "system_test",
      "BOTSTER_CONFIG_DIR" => temp_dir,
      "BOTSTER_SERVER_URL" => server_url,
      "BOTSTER_TOKEN" => api_key,  # Use BOTSTER_TOKEN (takes precedence over BOTSTER_API_KEY)
      "BOTSTER_HUB_ID" => hub.identifier,  # Use string identifier for find_or_initialize_by lookup
      "BOTSTER_REPO" => "test/repo",  # Optional — used for GitHub event subscription
      "RUST_LOG" => options[:log_level] || "info,botster=debug"
    }

    Rails.logger.info "[CliTestHelper] Starting CLI for hub #{hub.identifier}"
    Rails.logger.info "[CliTestHelper] Server URL: #{env['BOTSTER_SERVER_URL']}"
    Rails.logger.info "[CliTestHelper] BOTSTER_TOKEN set to: #{env['BOTSTER_TOKEN']&.slice(0, 20)}..."
    Rails.logger.debug "[CliTestHelper] Environment: #{env.except('BOTSTER_TOKEN')}"

    # Start CLI process
    stdout_r, stdout_w = IO.pipe
    stderr_r, stderr_w = IO.pipe

    # Run from temp_dir so the CLI doesn't find an existing .botster/ in the
    # real repo root. Init a bare git repo so the CLI can detect a repo for
    # heartbeat purposes. Config files go to temp_dir via BOTSTER_CONFIG_DIR.
    system("git", "init", "--quiet", temp_dir) unless File.exist?(File.join(temp_dir, ".git"))
    pid = spawn(
      env,
      CLI_BINARY.to_s,
      "start",
      "--headless",
      chdir: temp_dir,
      out: stdout_w,
      err: stderr_w
    )

    stdout_w.close
    stderr_w.close

    # Store log file path for debugging
    log_file_path = File.join(temp_dir, "botster.log")
    Rails.logger.info "[CliTestHelper] CLI log file: #{log_file_path}"

    # Capture start time for heartbeat-based readiness detection
    started_at = Time.current

    cli = CliProcess.new(
      pid: pid,
      hub: hub,
      stdout_r: stdout_r,
      stderr_r: stderr_r,
      temp_dir: temp_dir,
      log_thread: nil,
      log_file_path: log_file_path,
      device_token: device_token,
      started_at: started_at
    )

    # Start log reader thread
    log_thread = Thread.new do
      combined = IO.select([ stdout_r, stderr_r ])
      while cli.running?
        ready = IO.select([ stdout_r, stderr_r ], nil, nil, 0.1)
        next unless ready

        ready[0].each do |io|
          begin
            line = io.read_nonblock(4096)
            cli.add_output(line)
            Rails.logger.debug "[CLI] #{line}" if options[:verbose]
          rescue IO::WaitReadable, EOFError
            # Expected
          end
        end
      end
    end

    cli.instance_variable_set(:@log_thread, log_thread)

    # Wait for CLI to be ready
    timeout = options[:timeout] || 15
    unless cli.wait_for_ready(timeout: timeout)
      output = cli.recent_output
      log_output = cli.log_contents
      cli.stop
      raise "CLI failed to start within #{timeout}s.\nRecent stdout:\n#{output}\n\nRecent logs:\n#{log_output}"
    end

    Rails.logger.info "[CliTestHelper] CLI ready, connection URL available"
    cli
  end

  # Stop a CLI instance
  def stop_cli(cli)
    return unless cli

    Rails.logger.info "[CliTestHelper] Stopping CLI for hub #{cli.hub.identifier}"
    cli.stop
  end

  # Create a test hub with proper fixtures
  def create_test_hub(user: nil)
    user ||= users(:one)

    # Randomize hub ID sequence so each test gets a unique database ID.
    # Fixture loading resets Postgres sequences via TRUNCATE RESTART IDENTITY,
    # causing all tests to get the same hub ID. SharedWorker sessions (keyed by
    # hub ID) persist across test page loads and cause stale session collisions.
    # Use SecureRandom (not rand) because Minitest's srand(seed) makes Kernel#rand
    # deterministic — repeated seeds can produce duplicate IDs across tests.
    ActiveRecord::Base.connection.execute(
      "SELECT setval('hubs_id_seq', #{SecureRandom.random_number(2_000_000_000)}, false)"
    )

    Hub.create!(
      user: user,
      identifier: "test-hub-#{SecureRandom.hex(8)}",
      last_seen_at: Time.current
    )
  end

  private

  def server_url
    # Use the Capybara server URL
    host = Capybara.current_session.server.host
    port = Capybara.current_session.server.port
    "http://#{host}:#{port}"
  rescue
    # Fallback if Capybara server not available yet
    "http://127.0.0.1:#{ENV.fetch('PORT', 3000)}"
  end

  def create_device_token_for_hub(hub)
    # Create a device token for the CLI to authenticate
    # Returns the DeviceToken record (not just the token string) for cleanup
    # DeviceToken now belongs to Device, so we need to create a device first
    name = "CLI Test Token #{SecureRandom.hex(4)}"
    device = hub.user.devices.create!(
      name: name,
      device_type: "cli",
      fingerprint: SecureRandom.hex(8).scan(/../).join(":")
    )
    device.create_device_token!(name: name)
  end

  def cli_binary_current?
    return false unless File.exist?(CLI_BINARY)

    binary_mtime = File.mtime(CLI_BINARY)

    # Check source files and build config
    source_files = Dir.glob(CLI_PATH.join("src/**/*.rs")) +
                   Dir.glob(CLI_PATH.join("Cargo.{toml,lock}"))
    source_files.all? { |f| File.mtime(f) <= binary_mtime }
  rescue Errno::ENOENT
    false
  end

  def skip_build?
    ENV["SKIP_CLI_BUILD"] == "1"
  end
end
