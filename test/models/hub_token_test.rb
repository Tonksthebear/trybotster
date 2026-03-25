# frozen_string_literal: true

require "test_helper"

class HubTokenTest < ActiveSupport::TestCase
  setup do
    @user = User.create!(
      email: "hub_token_test@example.com",
      username: "hub_token_test"
    )
    @hub = @user.hubs.create!(
      identifier: "hub-token-test-#{SecureRandom.hex(8)}",
      last_seen_at: Time.current
    )
  end

  teardown do
    @user&.destroy
  end

  test "generates token with btstr_ prefix" do
    token = @hub.create_hub_token!

    assert token.token.start_with?("btstr_")
  end

  test "token is unique" do
    token1 = @hub.create_hub_token!
    hub2 = @user.hubs.create!(
      identifier: "hub-token-test-2-#{SecureRandom.hex(8)}",
      last_seen_at: Time.current
    )
    token2 = hub2.create_hub_token!

    assert_not_equal token1.token, token2.token
  end

  test "token is long enough for security" do
    token = @hub.create_hub_token!

    # btstr_ prefix (6) + 32 bytes base64 (~43 chars)
    assert token.token.length >= 40
  end

  test "display_token shows prefix and last 8 chars" do
    token = @hub.create_hub_token!
    display = token.display_token

    assert display.start_with?("btstr_...")
    assert_equal token.token.last(8), display.last(8)
  end

  test "touch_usage! updates last_used_at and last_ip" do
    token = @hub.create_hub_token!
    assert_nil token.last_used_at
    assert_nil token.last_ip

    token.touch_usage!(ip: "192.168.1.1")

    assert_not_nil token.last_used_at
    assert_equal "192.168.1.1", token.last_ip
  end

  test "requires hub" do
    token = HubToken.new

    assert_not token.valid?
    assert_includes token.errors[:hub], "must exist"
  end

  test "name is optional" do
    token = @hub.create_hub_token!

    assert token.valid?
    assert_nil token.name
  end

  test "can have a name" do
    token = @hub.create_hub_token!(name: "MacBook Pro")

    assert_equal "MacBook Pro", token.name
  end

  test "user convenience method returns hub owner" do
    token = @hub.create_hub_token!

    assert_equal @user, token.user
  end
end
