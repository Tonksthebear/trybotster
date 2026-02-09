# frozen_string_literal: true

require "test_helper"

class Integrations::Github::MCPTokenTest < ActiveSupport::TestCase
  setup do
    @user = User.create!(
      email: "mcp_token_test@example.com",
      username: "mcp_token_test"
    )
    @device = @user.devices.create!(
      name: "Test Device",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8).scan(/../).join(":")
    )
  end

  teardown do
    @user&.destroy
  end

  test "generates token with btmcp_ prefix" do
    token = @device.create_mcp_token!

    assert token.token.start_with?("btmcp_")
  end

  test "token is unique" do
    token1 = @device.create_mcp_token!
    device2 = @user.devices.create!(
      name: "Device 2",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8).scan(/../).join(":")
    )
    token2 = device2.create_mcp_token!

    assert_not_equal token1.token, token2.token
  end

  test "token is long enough for security" do
    token = @device.create_mcp_token!

    # btmcp_ prefix (6) + 32 bytes base64 (~43 chars)
    assert token.token.length >= 40
  end

  test "display_token shows prefix and last 8 chars" do
    token = @device.create_mcp_token!
    display = token.display_token

    assert display.start_with?("btmcp_...")
    assert_equal token.token.last(8), display.last(8)
  end

  test "touch_usage! updates last_used_at and last_ip" do
    token = @device.create_mcp_token!
    assert_nil token.last_used_at
    assert_nil token.last_ip

    token.touch_usage!(ip: "192.168.1.1")

    assert_not_nil token.last_used_at
    assert_equal "192.168.1.1", token.last_ip
  end

  test "requires device" do
    token = Integrations::Github::MCPToken.new

    assert_not token.valid?
    assert_includes token.errors[:device], "must exist"
  end

  test "user convenience method returns device owner" do
    token = @device.create_mcp_token!

    assert_equal @user, token.user
  end
end
