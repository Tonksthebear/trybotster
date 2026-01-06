# frozen_string_literal: true

require "test_helper"

class DeviceTokenTest < ActiveSupport::TestCase
  setup do
    @user = User.create!(
      email: "device_token_test@example.com",
      username: "device_token_test"
    )
  end

  teardown do
    @user&.destroy
  end

  test "generates token with btstr_ prefix" do
    token = @user.device_tokens.create!

    assert token.token.start_with?("btstr_")
  end

  test "token is unique" do
    token1 = @user.device_tokens.create!
    token2 = @user.device_tokens.create!

    assert_not_equal token1.token, token2.token
  end

  test "token is long enough for security" do
    token = @user.device_tokens.create!

    # btstr_ prefix (6) + 32 bytes base64 (~43 chars)
    assert token.token.length >= 40
  end

  test "display_token shows prefix and last 8 chars" do
    token = @user.device_tokens.create!
    display = token.display_token

    assert display.start_with?("btstr_...")
    assert_equal token.token.last(8), display.last(8)
  end

  test "touch_usage! updates last_used_at and last_ip" do
    token = @user.device_tokens.create!
    assert_nil token.last_used_at
    assert_nil token.last_ip

    token.touch_usage!(ip: "192.168.1.1")

    assert_not_nil token.last_used_at
    assert_equal "192.168.1.1", token.last_ip
  end

  test "requires user" do
    token = DeviceToken.new

    assert_not token.valid?
    assert_includes token.errors[:user], "must exist"
  end

  test "name is optional" do
    token = @user.device_tokens.create!

    assert token.valid?
    assert_nil token.name
  end

  test "can have a name" do
    token = @user.device_tokens.create!(name: "MacBook Pro")

    assert_equal "MacBook Pro", token.name
  end
end
