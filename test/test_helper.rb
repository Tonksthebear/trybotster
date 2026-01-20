ENV["RAILS_ENV"] ||= "test"
require_relative "../config/environment"
require "rails/test_help"
require "webmock/minitest"

# Load support files
Dir[Rails.root.join("test/support/**/*.rb")].each { |f| require f }

# Allow localhost connections for ActionCable tests
WebMock.disable_net_connect!(allow_localhost: true)

# Clean up stale CLI processes from previous test runs
# This prevents orphaned botster-hub processes from consuming CPU
module CliProcessCleanup
  CLI_BINARY_NAME = "botster-hub"

  def self.cleanup_stale_processes
    # Find any running botster-hub processes
    pids = `pgrep -f #{CLI_BINARY_NAME} 2>/dev/null`.strip.split("\n").map(&:to_i).reject(&:zero?)
    return if pids.empty?

    puts "[CliProcessCleanup] Found #{pids.length} stale #{CLI_BINARY_NAME} processes: #{pids.join(', ')}"

    pids.each do |pid|
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

# Clean up before tests start
CliProcessCleanup.cleanup_stale_processes

# Clean up after all tests complete (at_exit runs after Minitest finishes)
at_exit do
  CliProcessCleanup.cleanup_stale_processes
  CliProcessCleanup.cleanup_test_config_dir
end

# Set default host for URL generation in tests
Rails.application.routes.default_url_options[:host] = "test.host"

module ActiveSupport
  class TestCase
    # Run tests in parallel with specified workers
    parallelize(workers: :number_of_processors)

    # Setup all fixtures in test/fixtures/*.yml for all tests in alphabetical order.
    fixtures :all

    # Add more helper methods to be used by all tests here...
  end
end

# Helper module for mocking class methods in tests
module MockHelper
  def self.mock_tunnel_response_store(return_value, &block)
    original_wait_for = TunnelResponseStore.method(:wait_for)
    original_broadcast = ActionCable.server.method(:broadcast)

    TunnelResponseStore.define_singleton_method(:wait_for) do |_request_id, timeout: 30|
      return_value
    end

    # Also mock ActionCable.server.broadcast to do nothing
    ActionCable.server.define_singleton_method(:broadcast) do |*_args|
      true
    end

    block.call
  ensure
    TunnelResponseStore.define_singleton_method(:wait_for, original_wait_for)
    ActionCable.server.define_singleton_method(:broadcast, original_broadcast)
  end
end
