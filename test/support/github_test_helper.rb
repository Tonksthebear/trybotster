# frozen_string_literal: true

require "ostruct"

# Helper module for mocking Octokit/GitHub API responses in tests.
#
# Octokit returns Sawyer::Resource objects which support:
# - Dot notation: resource.html_url
# - Bracket access: resource[:html_url]
# - to_h method: resource.to_h
#
# This helper provides mock objects that behave like Sawyer::Resource
# and stub methods for Github::App class methods.
#
# Usage:
#   class MyTest < ActionDispatch::IntegrationTest
#     include GithubTestHelper
#
#     test "something with github" do
#       with_stubbed_github do
#         # ... test code that calls Github::App methods ...
#       end
#     end
#   end
#
module GithubTestHelper
  extend ActiveSupport::Concern

  # Creates a mock object that behaves like Sawyer::Resource.
  # Supports dot notation, bracket access, and to_h.
  #
  # @param attrs [Hash] Attributes for the resource
  # @return [Object] Mock resource object
  #
  # @example
  #   comment = sawyer_resource(html_url: "https://github.com/...", id: 123)
  #   comment.html_url      # => "https://github.com/..."
  #   comment[:html_url]    # => "https://github.com/..."
  #   comment.to_h          # => { html_url: "https://...", id: 123 }
  #
  def sawyer_resource(attrs = {})
    resource = OpenStruct.new(attrs)

    # Add bracket access like Sawyer::Resource
    resource.define_singleton_method(:[]) do |key|
      attrs[key.to_sym] || attrs[key.to_s]
    end

    # Add to_h method that returns symbolized keys (like Sawyer)
    resource.define_singleton_method(:to_h) do
      attrs.transform_keys(&:to_sym)
    end

    resource
  end

  # Creates a mock Octokit client with stubbed methods.
  #
  # @param stubs [Hash] Method names to return values
  # @return [Object] Mock client object
  #
  # @example
  #   client = mock_octokit_client(
  #     add_comment: sawyer_resource(html_url: "..."),
  #     user: sawyer_resource(login: "testuser")
  #   )
  #
  def mock_octokit_client(stubs = {})
    client = Object.new

    stubs.each do |method_name, return_value|
      client.define_singleton_method(method_name) { |*_args| return_value }
    end

    client
  end

  # Stubs both installation lookup and client for a complete GitHub flow.
  # This is the most common helper for testing GitHub-dependent endpoints.
  #
  # Uses the same pattern as MockHelper - temporarily replaces class methods
  # using define_singleton_method, then restores originals after the block.
  #
  # @param installation_id [Integer] Installation ID (default: 12345)
  # @param comment_url [String] Comment URL for responses
  # @yield Block where stubs are active
  #
  # @example
  #   with_stubbed_github do
  #     post hub_notifications_url(@hub.identifier), params: {...}
  #     assert_response :created
  #   end
  #
  def with_stubbed_github(installation_id: 12345, comment_url: "https://github.com/owner/repo/issues/1#issuecomment-123")
    # Save original methods
    original_get_installation = Github::App.method(:get_installation_for_repo)
    original_installation_client = Github::App.method(:installation_client)

    # Create mock response for get_installation_for_repo
    installation_response = {
      success: true,
      installation_id: installation_id,
      account: "owner",
      account_type: "Organization"
    }

    # Create mock client for installation_client
    mock_comment = sawyer_resource(
      html_url: comment_url,
      id: 123,
      body: "test comment"
    )
    mock_client = mock_octokit_client(add_comment: mock_comment)

    # Replace with stubs
    Github::App.define_singleton_method(:get_installation_for_repo) do |*_args|
      installation_response
    end

    Github::App.define_singleton_method(:installation_client) do |*_args|
      mock_client
    end

    yield
  ensure
    # Restore original methods
    Github::App.define_singleton_method(:get_installation_for_repo, original_get_installation)
    Github::App.define_singleton_method(:installation_client, original_installation_client)
  end
end
