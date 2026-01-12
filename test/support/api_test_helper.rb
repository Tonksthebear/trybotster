# frozen_string_literal: true

# Helper module for API request tests.
#
# Provides authentication helpers and common assertions for testing
# the Rails API endpoints that the CLI calls.
#
# Usage in request tests:
#
#   class DevicesControllerTest < ActionDispatch::IntegrationTest
#     include ApiTestHelper
#
#     test "list devices with valid token" do
#       get devices_url, headers: auth_headers_for(:jason)
#       assert_response :ok
#       assert_json_response
#     end
#   end
#
module ApiTestHelper
  extend ActiveSupport::Concern

  included do
    # Make fixtures available
    fixtures :users, :hubs
  end

  # Returns authorization headers for a user fixture.
  # Creates a device token dynamically to avoid encrypted attribute issues.
  #
  # @param user_fixture [Symbol] Name of the user fixture
  # @return [Hash] Headers hash with Authorization bearer token
  #
  # @example
  #   get devices_url, headers: auth_headers_for(:jason)
  #
  def auth_headers_for(user_fixture)
    user = users(user_fixture)
    @_api_tokens ||= {}
    @_api_tokens[user_fixture] ||= user.device_tokens.create!(name: "Test Token")

    {
      "Authorization" => "Bearer #{@_api_tokens[user_fixture].token}",
      "Content-Type" => "application/json",
      "Accept" => "application/json"
    }
  end

  # Alias for backwards compatibility
  def auth_headers(user_fixture = :jason)
    auth_headers_for(user_fixture)
  end

  # Returns headers without authentication (for unauthenticated endpoints).
  def json_headers
    {
      "Content-Type" => "application/json",
      "Accept" => "application/json"
    }
  end

  # Asserts that the response is valid JSON and returns the parsed body.
  #
  # @return [Hash, Array] Parsed JSON response
  def assert_json_response
    assert_equal "application/json; charset=utf-8", response.content_type
    JSON.parse(response.body)
  end

  # Asserts the response contains expected keys.
  #
  # @param keys [Array<String, Symbol>] Expected top-level keys
  def assert_json_keys(*keys)
    json = assert_json_response
    keys.flatten.each do |key|
      assert json.key?(key.to_s), "Expected JSON to have key '#{key}', got: #{json.keys}"
    end
    json
  end

  # Asserts the response is an error with the given message.
  #
  # @param expected_error [String, Regexp] Expected error message
  def assert_json_error(expected_error = nil)
    json = assert_json_response
    assert json.key?("error"), "Expected JSON to have 'error' key, got: #{json.keys}"

    if expected_error.is_a?(Regexp)
      assert_match expected_error, json["error"]
    elsif expected_error
      assert_equal expected_error, json["error"]
    end

    json
  end

  # Creates a device token for a user and returns auth headers.
  # Useful when you need a fresh token not from fixtures.
  #
  # @param user [User, Symbol] User record or fixture name
  # @return [Hash] Headers hash with Authorization bearer token
  def create_token_headers(user)
    user = users(user) if user.is_a?(Symbol)
    token = user.device_tokens.create!(name: "Test Token #{SecureRandom.hex(4)}")
    {
      "Authorization" => "Bearer #{token.token}",
      "Content-Type" => "application/json",
      "Accept" => "application/json"
    }
  end
end
