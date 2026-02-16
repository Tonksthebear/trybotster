# frozen_string_literal: true

require "test_helper"

class DeviceTest < ActiveSupport::TestCase
  setup do
    @user = User.create!(
      email: "device_test_user@example.com",
      username: "device_test_user"
    )
  end

  teardown do
    @user&.destroy
  end

  # --- Valid records ---

  test "valid CLI device with public key" do
    device = @user.devices.new(
      name: "My CLI",
      device_type: "cli",
      public_key: "cli_public_key_for_valid_test"
    )
    assert device.valid?
    assert device.fingerprint.present?, "fingerprint should be generated"
  end

  test "valid CLI device without public key (secure mode)" do
    device = @user.devices.new(
      name: "Secure CLI",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    assert device.valid?
  end

  test "valid browser device with public key" do
    device = @user.devices.new(
      name: "Chrome",
      device_type: "browser",
      public_key: "browser_public_key_for_valid_test"
    )
    assert device.valid?
  end

  # --- Validation: name ---

  test "requires name" do
    device = @user.devices.new(
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    assert_not device.valid?
    assert_includes device.errors[:name], "can't be blank"
  end

  # --- Validation: device_type ---

  test "requires device_type" do
    device = @user.devices.new(
      name: "Test",
      fingerprint: SecureRandom.hex(8)
    )
    assert_not device.valid?
    assert_includes device.errors[:device_type], "can't be blank"
  end

  test "device_type must be cli or browser" do
    device = @user.devices.new(
      name: "Test",
      device_type: "mobile",
      fingerprint: SecureRandom.hex(8)
    )
    assert_not device.valid?
    assert_includes device.errors[:device_type], "is not included in the list"
  end

  test "device_type cli is valid" do
    device = @user.devices.new(
      name: "CLI",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    assert device.valid?
  end

  test "device_type browser is valid" do
    device = @user.devices.new(
      name: "Browser",
      device_type: "browser",
      public_key: "browser_device_type_test_key"
    )
    assert device.valid?
  end

  # --- Validation: public_key ---

  test "browser device requires public_key" do
    device = @user.devices.new(
      name: "Chrome",
      device_type: "browser",
      fingerprint: SecureRandom.hex(8)
    )
    assert_not device.valid?
    assert_includes device.errors[:public_key], "can't be blank"
  end

  test "CLI device does not require public_key" do
    device = @user.devices.new(
      name: "CLI",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    assert device.valid?
  end

  test "public_key must be unique when present" do
    @user.devices.create!(
      name: "First",
      device_type: "cli",
      public_key: "shared_unique_key_test"
    )

    duplicate = @user.devices.new(
      name: "Second",
      device_type: "cli",
      public_key: "shared_unique_key_test"
    )
    assert_not duplicate.valid?
    assert_includes duplicate.errors[:public_key], "has already been taken"
  ensure
    @user.devices.where(public_key: "shared_unique_key_test").destroy_all
  end

  test "multiple CLI devices can have nil public_key" do
    d1 = @user.devices.create!(
      name: "Secure CLI 1",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    d2 = @user.devices.new(
      name: "Secure CLI 2",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    assert d2.valid?
  ensure
    d1&.destroy
  end

  # --- Validation: fingerprint ---

  test "requires fingerprint" do
    # Build without public_key so generate_fingerprint won't fire, and no manual fingerprint
    device = @user.devices.new(
      name: "No FP",
      device_type: "cli"
    )
    assert_not device.valid?
    assert_includes device.errors[:fingerprint], "can't be blank"
  end

  test "fingerprint must be unique per user" do
    fp = "de:ad:be:ef:ca:fe:00:01"
    @user.devices.create!(
      name: "First",
      device_type: "cli",
      fingerprint: fp
    )

    duplicate = @user.devices.new(
      name: "Second",
      device_type: "cli",
      fingerprint: fp
    )
    assert_not duplicate.valid?
    assert_includes duplicate.errors[:fingerprint], "has already been taken"
  ensure
    @user.devices.where(fingerprint: fp).destroy_all
  end

  test "same fingerprint allowed for different users" do
    other_user = User.create!(
      email: "other_device_test@example.com",
      username: "other_device_test"
    )
    fp = "de:ad:be:ef:ca:fe:00:02"

    @user.devices.create!(
      name: "User1 Device",
      device_type: "cli",
      fingerprint: fp
    )

    device = other_user.devices.new(
      name: "User2 Device",
      device_type: "cli",
      fingerprint: fp
    )
    assert device.valid?
  ensure
    other_user&.destroy
  end

  # --- Fingerprint generation ---

  test "generate_fingerprint creates colon-separated hex from SHA256 of public_key" do
    public_key = "test_fingerprint_generation_key"
    device = @user.devices.new(
      name: "FP Test",
      device_type: "cli",
      public_key: public_key
    )
    device.valid? # triggers before_validation

    expected_hash = Digest::SHA256.digest(public_key)[0, 8]
    expected_fp = expected_hash.bytes.map { |b| b.to_s(16).rjust(2, "0") }.join(":")

    assert_equal expected_fp, device.fingerprint
  end

  test "generate_fingerprint is 8 colon-separated hex bytes" do
    device = @user.devices.new(
      name: "Format Test",
      device_type: "cli",
      public_key: "format_test_key_content"
    )
    device.valid?

    parts = device.fingerprint.split(":")
    assert_equal 8, parts.length
    parts.each do |part|
      assert_match(/\A[0-9a-f]{2}\z/, part)
    end
  end

  test "generate_fingerprint skipped when public_key is blank" do
    device = @user.devices.new(
      name: "No Key",
      device_type: "cli"
    )
    device.valid?
    assert_nil device.fingerprint
  end

  test "generate_fingerprint only runs on create" do
    device = @user.devices.create!(
      name: "Persist Test",
      device_type: "cli",
      public_key: "persist_fingerprint_key"
    )
    original_fp = device.fingerprint

    # Updating public_key should NOT regenerate fingerprint
    device.update!(name: "Renamed")
    assert_equal original_fp, device.reload.fingerprint
  ensure
    device&.destroy
  end

  # --- Scopes ---

  test "cli_devices scope returns only CLI devices" do
    cli = @user.devices.create!(name: "CLI", device_type: "cli", fingerprint: SecureRandom.hex(8))
    browser = @user.devices.create!(name: "Browser", device_type: "browser", public_key: "scope_cli_browser_key")

    result = @user.devices.cli_devices
    assert_includes result, cli
    assert_not_includes result, browser
  ensure
    cli&.destroy
    browser&.destroy
  end

  test "browser_devices scope returns only browser devices" do
    cli = @user.devices.create!(name: "CLI", device_type: "cli", fingerprint: SecureRandom.hex(8))
    browser = @user.devices.create!(name: "Browser", device_type: "browser", public_key: "scope_browser_test_key")

    result = @user.devices.browser_devices
    assert_includes result, browser
    assert_not_includes result, cli
  ensure
    cli&.destroy
    browser&.destroy
  end

  test "active scope returns devices seen within 5 minutes" do
    active = @user.devices.create!(name: "Active", device_type: "cli", fingerprint: SecureRandom.hex(8), last_seen_at: 1.minute.ago)
    stale = @user.devices.create!(name: "Stale", device_type: "cli", fingerprint: SecureRandom.hex(8), last_seen_at: 10.minutes.ago)
    never_seen = @user.devices.create!(name: "Never", device_type: "cli", fingerprint: SecureRandom.hex(8), last_seen_at: nil)

    result = @user.devices.active
    assert_includes result, active
    assert_not_includes result, stale
    assert_not_includes result, never_seen
  ensure
    active&.destroy
    stale&.destroy
    never_seen&.destroy
  end

  test "by_last_seen scope orders by last_seen_at descending" do
    old = @user.devices.create!(name: "Old", device_type: "cli", fingerprint: SecureRandom.hex(8), last_seen_at: 1.hour.ago)
    recent = @user.devices.create!(name: "Recent", device_type: "cli", fingerprint: SecureRandom.hex(8), last_seen_at: 1.minute.ago)
    mid = @user.devices.create!(name: "Mid", device_type: "cli", fingerprint: SecureRandom.hex(8), last_seen_at: 30.minutes.ago)

    result = @user.devices.by_last_seen.to_a
    recent_idx = result.index(recent)
    mid_idx = result.index(mid)
    old_idx = result.index(old)

    assert recent_idx < mid_idx, "recent should come before mid"
    assert mid_idx < old_idx, "mid should come before old"
  ensure
    old&.destroy
    recent&.destroy
    mid&.destroy
  end

  # --- Instance methods ---

  test "cli? returns true for CLI device" do
    device = Device.new(device_type: "cli")
    assert device.cli?
  end

  test "cli? returns false for browser device" do
    device = Device.new(device_type: "browser")
    assert_not device.cli?
  end

  test "browser? returns true for browser device" do
    device = Device.new(device_type: "browser")
    assert device.browser?
  end

  test "browser? returns false for CLI device" do
    device = Device.new(device_type: "cli")
    assert_not device.browser?
  end

  test "active? returns true when last_seen_at is within 5 minutes" do
    device = Device.new(last_seen_at: 2.minutes.ago)
    assert device.active?
  end

  test "active? returns false when last_seen_at is older than 5 minutes" do
    device = Device.new(last_seen_at: 10.minutes.ago)
    assert_not device.active?
  end

  test "active? returns false when last_seen_at is nil" do
    device = Device.new(last_seen_at: nil)
    assert_not device.active?
  end

  test "active? returns false when last_seen_at is exactly 5 minutes ago" do
    device = Device.new(last_seen_at: 5.minutes.ago)
    assert_not device.active?
  end

  test "secure_mode? returns true for CLI device without public_key" do
    device = Device.new(device_type: "cli", public_key: nil)
    assert device.secure_mode?
  end

  test "secure_mode? returns false for CLI device with public_key" do
    device = Device.new(device_type: "cli", public_key: "some_key")
    assert_not device.secure_mode?
  end

  test "secure_mode? returns false for browser device without public_key" do
    device = Device.new(device_type: "browser", public_key: nil)
    assert_not device.secure_mode?
  end

  test "touch_last_seen! updates last_seen_at without running validations" do
    device = @user.devices.create!(
      name: "Touch Test",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    assert_nil device.last_seen_at

    freeze_time do
      device.touch_last_seen!
      assert_equal Time.current, device.reload.last_seen_at
    end
  ensure
    device&.destroy
  end

  # --- Associations ---

  test "belongs to user" do
    device = devices(:cli_device)
    assert_equal users(:jason), device.user
  end

  test "has many hubs with destroy" do
    device = @user.devices.create!(
      name: "Hub Owner",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    hub = Hub.create!(
      user: @user,
      device: device,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )

    device.destroy
    assert_not Hub.exists?(hub.id), "Hub should be destroyed with device"
  end

  # --- Fixtures ---

  test "fixtures are valid" do
    assert devices(:cli_device).valid?
    assert devices(:browser_device).valid?
  end
end
