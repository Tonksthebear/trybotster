# frozen_string_literal: true

require "test_helper"

class UserTest < ActiveSupport::TestCase
  setup do
    @user = users(:jason)
  end

  # === Validations ===

  test "valid user" do
    user = User.new(email: "new@example.com", username: "newuser")
    assert user.valid?
  end

  test "requires email for non-OAuth user" do
    user = User.new(email: nil, username: "nomail")
    assert_not user.valid?
    assert_includes user.errors[:email], "can't be blank"
  end

  test "email must be unique" do
    duplicate = User.new(email: @user.email, username: "dup")
    assert_not duplicate.valid?
  end

  # === API Key ===

  test "generates api_key before create" do
    user = User.create!(email: "apikey@example.com", username: "apikey")
    assert user.api_key.present?
  ensure
    user&.destroy
  end

  test "regenerate_api_key! changes the key" do
    old_key = @user.api_key
    @user.regenerate_api_key!
    assert_not_equal old_key, @user.api_key
  end

  # === GitHub App Token ===

  test "github_app_authorized? returns true when token present" do
    @user.update!(github_app_token: "ghu_test_token")
    assert @user.github_app_authorized?
  end

  test "github_app_authorized? returns false when no token" do
    @user.update!(github_app_token: nil)
    assert_not @user.github_app_authorized?
  end

  test "github_app_token_expired? returns true when expires_at is in the past" do
    @user.update!(github_app_token_expires_at: 1.hour.ago)
    assert @user.github_app_token_expired?
  end

  test "github_app_token_expired? returns false when expires_at is in the future" do
    @user.update!(github_app_token_expires_at: 1.hour.from_now)
    assert_not @user.github_app_token_expired?
  end

  test "github_app_token_expired? returns true when no expiry set" do
    @user.update!(github_app_token_expires_at: nil)
    assert @user.github_app_token_expired?
  end

  test "github_app_token_needs_refresh? returns true within 1 hour of expiry" do
    @user.update!(github_app_token_expires_at: 30.minutes.from_now)
    assert @user.github_app_token_needs_refresh?
  end

  test "github_app_token_needs_refresh? returns false when more than 1 hour from expiry" do
    @user.update!(github_app_token_expires_at: 2.hours.from_now)
    assert_not @user.github_app_token_needs_refresh?
  end

  test "revoke_github_app_authorization! clears all token fields" do
    @user.update!(
      github_app_token: "ghu_test",
      github_app_refresh_token: "ghr_test",
      github_app_token_expires_at: 1.hour.from_now
    )

    @user.revoke_github_app_authorization!
    @user.reload

    assert_nil @user.github_app_token
    assert_nil @user.github_app_refresh_token
    assert_nil @user.github_app_token_expires_at
  end

  # === Associations ===

  test "has many hubs" do
    assert_respond_to @user, :hubs
  end

  test "has many devices" do
    assert_respond_to @user, :devices
  end

  test "destroying user destroys hubs" do
    user = User.create!(email: "destroy@example.com", username: "destroyer")
    Hub.create!(user: user, identifier: "doomed-hub", last_seen_at: Time.current)

    assert_difference "Hub.count", -1 do
      user.destroy
    end
  end
end
