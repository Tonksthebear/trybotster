# frozen_string_literal: true

require "test_helper"

class DeviceAuthorizationTest < ActiveSupport::TestCase
  setup do
    @user = User.create!(
      email: "device_auth_test@example.com",
      username: "device_auth_test"
    )
  end

  teardown do
    DeviceAuthorization.where(user: @user).destroy_all
    DeviceAuthorization.where(user: nil).destroy_all
    @user&.destroy
  end

  test "generates device_code and user_code on create" do
    auth = DeviceAuthorization.create!

    assert auth.device_code.present?
    assert auth.user_code.present?
    assert_equal "pending", auth.status
  end

  test "device_code is unique and url-safe" do
    auth = DeviceAuthorization.create!

    # Should be url-safe base64
    assert_match(/\A[A-Za-z0-9_-]+\z/, auth.device_code)
    assert auth.device_code.length >= 32
  end

  test "user_code uses only unambiguous characters" do
    auth = DeviceAuthorization.create!

    # Should not contain ambiguous characters: 0/O, 1/I/L, 5/S, A/4, U/V
    assert_no_match(/[0O1IL5SAUV]/, auth.user_code)
    assert_equal 8, auth.user_code.length
  end

  test "formatted_user_code adds hyphen" do
    auth = DeviceAuthorization.create!

    formatted = auth.formatted_user_code
    assert_match(/\A[A-Z0-9]{4}-[A-Z0-9]{4}\z/, formatted)
  end

  test "sets default expiration" do
    auth = DeviceAuthorization.create!

    assert auth.expires_at.present?
    # Should expire in about 15 minutes
    assert auth.expires_at > 14.minutes.from_now
    assert auth.expires_at < 16.minutes.from_now
  end

  test "expired? returns true for past expiration" do
    auth = DeviceAuthorization.create!(expires_at: 1.hour.ago)

    assert auth.expired?
  end

  test "expired? returns false for future expiration" do
    auth = DeviceAuthorization.create!

    assert_not auth.expired?
  end

  test "approve! sets user and status" do
    auth = DeviceAuthorization.create!
    auth.approve!(@user)

    assert_equal @user, auth.user
    assert_equal "approved", auth.status
  end

  test "deny! sets status to denied" do
    auth = DeviceAuthorization.create!
    auth.deny!

    assert_equal "denied", auth.status
  end

  test "expire! sets status to expired" do
    auth = DeviceAuthorization.create!
    auth.expire!

    assert_equal "expired", auth.status
  end

  test "valid_pending scope returns only pending non-expired" do
    pending = DeviceAuthorization.create!
    expired = DeviceAuthorization.create!(expires_at: 1.hour.ago)
    approved = DeviceAuthorization.create!
    approved.approve!(@user)

    valid = DeviceAuthorization.valid_pending

    assert_includes valid, pending
    assert_not_includes valid, expired
    assert_not_includes valid, approved
  end

  test "cleanup_expired! marks expired pending as expired" do
    auth = DeviceAuthorization.create!(expires_at: 1.hour.ago)
    assert_equal "pending", auth.status

    DeviceAuthorization.cleanup_expired!

    auth.reload
    assert_equal "expired", auth.status
  end
end
