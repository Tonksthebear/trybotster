ENV["RAILS_ENV"] ||= "test"
require_relative "../config/environment"
require "rails/test_help"
require "webmock/minitest"

# Allow localhost connections for ActionCable tests
WebMock.disable_net_connect!(allow_localhost: true)

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
