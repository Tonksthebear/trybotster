ENV["RAILS_ENV"] ||= "test"
require_relative "../config/environment"
require "rails/test_help"
require "webmock/minitest"

# Load support files
Dir[Rails.root.join("test/support/**/*.rb")].each { |f| require f }

# Allow localhost connections for ActionCable tests
WebMock.disable_net_connect!(allow_localhost: true)

# Clean up stale CLI processes from previous test runs.
# Only kills processes whose PIDs were recorded in the tracking file
# by CliTestHelper#start_cli — never touches a developer's real hub.
module CliProcessCleanup
  PID_FILE = Rails.root.join("tmp/pids/cli_test_pids.txt").freeze

  def self.record_pid(pid)
    FileUtils.mkdir_p(PID_FILE.dirname)
    File.open(PID_FILE, "a") { |f| f.puts(pid) }
  end

  def self.cleanup_stale_processes
    return unless File.exist?(PID_FILE)

    pids = File.readlines(PID_FILE).map(&:strip).reject(&:empty?).map(&:to_i).reject(&:zero?).uniq
    living = pids.select { |pid| process_running?(pid) }

    if living.any?
      puts "[CliProcessCleanup] Found #{living.length} stale test-spawned processes: #{living.join(', ')}"
      living.each do |pid|
        begin
          Process.kill("TERM", pid)
          sleep 0.2
          Process.kill("KILL", pid) if process_running?(pid)
          puts "[CliProcessCleanup] Killed stale process #{pid}"
        rescue Errno::ESRCH, Errno::EPERM
          # Process already gone or permission denied
        end
      end
    end

    # Clear the tracking file
    File.delete(PID_FILE) if File.exist?(PID_FILE)
  end

  def self.cleanup_test_config_dir
    test_config_dir = Rails.root.join("tmp/botster-test")
    return unless test_config_dir.exist?

    FileUtils.rm_rf(test_config_dir)
    puts "[CliProcessCleanup] Cleaned up test config directory: #{test_config_dir}"
  rescue => e
    puts "[CliProcessCleanup] Warning: Failed to clean up test config: #{e.message}"
  end

  def self.process_running?(pid)
    Process.kill(0, pid)
    true
  rescue Errno::ESRCH, Errno::EPERM
    false
  end
end

# Set default host for URL generation in tests
Rails.application.routes.default_url_options[:host] = "test.host"

module ActiveSupport
  class TestCase
    include WaitHelper

    # Run tests in parallel with specified workers
    parallelize(workers: :number_of_processors)

    # Clean up stale CLI processes ONCE before forking workers.
    # This prevents orphaned processes from previous runs without
    # workers killing each other's processes during parallel testing.
    parallelize_before_fork do
      CliProcessCleanup.cleanup_stale_processes
    end

    # Clean up config dir after workers complete
    parallelize_teardown do |_worker|
      CliProcessCleanup.cleanup_test_config_dir
    end

    # Setup all fixtures in test/fixtures/*.yml for all tests in alphabetical order.
    set_fixture_class "integrations/github/mcp_tokens" => Integrations::Github::MCPToken
    fixtures :all

    # Add more helper methods to be used by all tests here...
  end
end
