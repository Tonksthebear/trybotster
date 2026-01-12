# frozen_string_literal: true

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
  CLI_PATH = Rails.root.join("cli").freeze
  CLI_BINARY = CLI_PATH.join("target/debug/botster-hub").freeze

  # Represents a running CLI instance
  class CliProcess
    attr_reader :pid, :hub, :temp_dir, :log_file_path
    attr_accessor :device_token

    def initialize(pid:, hub:, stdout_r:, stderr_r:, temp_dir:, log_thread:, log_file_path: nil, device_token: nil)
      @pid = pid
      @hub = hub
      @stdout_r = stdout_r
      @stderr_r = stderr_r
      @temp_dir = temp_dir
      @log_thread = log_thread
      @log_file_path = log_file_path
      @device_token = device_token
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
        # Give it a moment to clean up
        sleep 0.5
        # Force kill if still running
        Process.kill("KILL", @pid) if running?
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
      deadline = Time.current + timeout

      while Time.current < deadline
        return true if ready?
        sleep 0.2
      end

      false
    end

    def ready?
      # CLI is ready when connection URL file exists
      connection_url.present?
    end

    # Get the connection URL from the running CLI
    #
    # This URL includes the Signal PreKeyBundle in the fragment
    # and can be used to visit the hub page with encryption ready
    def connection_url
      return @cached_url if @cached_url

      # Must use same config dir as the running CLI to find the URL file
      env_prefix = "BOTSTER_CONFIG_DIR=#{@temp_dir}"
      result = `#{env_prefix} #{CLI_BINARY} get-connection-url --hub #{@hub.identifier} 2>/dev/null`.strip
      @cached_url = result if $?.success? && result.present?
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
    Rails.logger.info "[CliTestHelper] DeviceToken user_id=#{device_token.user_id}"

    # Set up environment
    # BOTSTER_ENV=test enables all test-specific behaviors:
    # - Skip authentication validation
    # - Use file storage instead of OS keyring
    env = {
      "BOTSTER_ENV" => "test",
      "BOTSTER_CONFIG_DIR" => temp_dir,
      "BOTSTER_SERVER_URL" => server_url,
      "BOTSTER_TOKEN" => api_key,  # Use BOTSTER_TOKEN (takes precedence over BOTSTER_API_KEY)
      "BOTSTER_HUB_ID" => hub.identifier,  # Use Rails hub identifier for consistency
      "RUST_LOG" => options[:log_level] || "info,botster_hub=debug"
    }

    Rails.logger.info "[CliTestHelper] Starting CLI for hub #{hub.identifier}"
    Rails.logger.info "[CliTestHelper] Server URL: #{env['BOTSTER_SERVER_URL']}"
    Rails.logger.info "[CliTestHelper] BOTSTER_TOKEN set to: #{env['BOTSTER_TOKEN']&.slice(0, 20)}..."
    Rails.logger.debug "[CliTestHelper] Environment: #{env.except('BOTSTER_TOKEN')}"

    # Start CLI process
    stdout_r, stdout_w = IO.pipe
    stderr_r, stderr_w = IO.pipe

    pid = spawn(
      env,
      CLI_BINARY.to_s,
      "start",
      "--headless",
      out: stdout_w,
      err: stderr_w,
      chdir: temp_dir
    )

    stdout_w.close
    stderr_w.close

    # Store log file path for debugging
    log_file_path = File.join(temp_dir, "botster-hub.log")
    Rails.logger.info "[CliTestHelper] CLI log file: #{log_file_path}"

    cli = CliProcess.new(
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
      combined = IO.select([stdout_r, stderr_r])
      while cli.running?
        ready = IO.select([stdout_r, stderr_r], nil, nil, 0.1)
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

    Hub.create!(
      user: user,
      identifier: "test-hub-#{SecureRandom.hex(8)}",
      repo: "test/repo",
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
    hub.user.device_tokens.create!(name: "CLI Test Token #{SecureRandom.hex(4)}")
  end

  def cli_binary_current?
    return false unless File.exist?(CLI_BINARY)

    binary_mtime = File.mtime(CLI_BINARY)

    # Check key source files
    source_files = Dir.glob(CLI_PATH.join("src/**/*.rs"))
    source_files.all? { |f| File.mtime(f) <= binary_mtime }
  rescue Errno::ENOENT
    false
  end

  def skip_build?
    ENV["SKIP_CLI_BUILD"] == "1"
  end
end
