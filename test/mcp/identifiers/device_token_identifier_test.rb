# frozen_string_literal: true

require "test_helper"
require "ostruct"

class DeviceTokenIdentifierTest < ActiveSupport::TestCase
  setup do
    @user = User.create!(
      email: "identifier_test@example.com",
      username: "identifier_test"
    )
    @device = @user.devices.create!(
      name: "Test Device",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8).scan(/../).join(":")
    )
    @mcp_token = @device.create_mcp_token!
  end

  teardown do
    @user&.destroy
  end

  # Create a mock request with the given Authorization header
  def mock_request(auth_header: nil, remote_ip: "127.0.0.1")
    env = {}
    env["HTTP_AUTHORIZATION"] = auth_header if auth_header

    request = OpenStruct.new(
      env: env,
      remote_ip: remote_ip
    )
    request
  end

  test "identifier is configured correctly" do
    assert_equal :user, DeviceTokenIdentifier.identifier_name
    assert_equal "api_key", DeviceTokenIdentifier.auth_method
  end

  test "resolves user from valid bearer token" do
    request = mock_request(auth_header: "Bearer #{@mcp_token.token}")

    identifier = DeviceTokenIdentifier.new(request)
    result = identifier.resolve

    assert_equal @user, result
  end

  test "returns nil when no authorization header" do
    request = mock_request(auth_header: nil)

    identifier = DeviceTokenIdentifier.new(request)
    result = identifier.resolve

    assert_nil result
  end

  test "returns nil when token not found in database" do
    request = mock_request(auth_header: "Bearer btmcp_nonexistent_token")

    identifier = DeviceTokenIdentifier.new(request)
    result = identifier.resolve

    assert_nil result
  end

  test "returns nil for empty bearer token" do
    request = mock_request(auth_header: "Bearer ")

    identifier = DeviceTokenIdentifier.new(request)
    result = identifier.resolve

    assert_nil result
  end

  test "returns nil for malformed authorization header" do
    request = mock_request(auth_header: "Basic dXNlcjpwYXNz")

    identifier = DeviceTokenIdentifier.new(request)
    result = identifier.resolve

    assert_nil result
  end

  test "updates last_used_at and last_ip on successful auth" do
    assert_nil @mcp_token.last_used_at
    assert_nil @mcp_token.last_ip

    request = mock_request(auth_header: "Bearer #{@mcp_token.token}", remote_ip: "192.168.1.100")

    identifier = DeviceTokenIdentifier.new(request)
    identifier.resolve

    @mcp_token.reload
    assert_not_nil @mcp_token.last_used_at
    assert_equal "192.168.1.100", @mcp_token.last_ip
  end
end
