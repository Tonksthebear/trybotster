# frozen_string_literal: true

require "test_helper"
require "turbo/broadcastable/test_helper"

class HubTest < ActiveSupport::TestCase
  include Turbo::Broadcastable::TestHelper

  setup do
    @user = User.create!(
      email: "hub_test_user@example.com",
      username: "hub_test_user"
    )
  end

  teardown do
    @user&.destroy
  end

  test "valid hub" do
    hub = Hub.new(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    assert hub.valid?
  end

  test "requires user" do
    hub = Hub.new(
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    assert_not hub.valid?
    assert_includes hub.errors[:user], "must exist"
  end

  test "requires identifier" do
    hub = Hub.new(
      user: @user,
      last_seen_at: Time.current
    )
    assert_not hub.valid?
    assert_includes hub.errors[:identifier], "can't be blank"
  end

  test "requires last_seen_at" do
    hub = Hub.new(
      user: @user,
      identifier: SecureRandom.uuid
    )
    assert_not hub.valid?
    assert_includes hub.errors[:last_seen_at], "can't be blank"
  end

  test "identifier must be unique" do
    identifier = SecureRandom.uuid
    Hub.create!(
      user: @user,
      identifier: identifier,
      last_seen_at: Time.current
    )

    duplicate = Hub.new(
      user: @user,
      identifier: identifier,
      last_seen_at: Time.current
    )
    assert_not duplicate.valid?
    assert_includes duplicate.errors[:identifier], "has already been taken"
  end

  test "active scope returns hubs that are alive and seen within 2 minutes" do
    active_hub = Hub.create!(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: 1.minute.ago,
      alive: true
    )
    stale_hub = Hub.create!(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: 5.minutes.ago,
      alive: true
    )
    dead_hub = Hub.create!(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: 1.minute.ago,
      alive: false
    )

    active_hubs = Hub.active
    assert_includes active_hubs, active_hub
    assert_not_includes active_hubs, stale_hub
    assert_not_includes active_hubs, dead_hub
  end

  test "stale scope returns hubs that are dead or not seen within 2 minutes" do
    active_hub = Hub.create!(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: 1.minute.ago,
      alive: true
    )
    stale_hub = Hub.create!(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: 5.minutes.ago,
      alive: true
    )
    dead_hub = Hub.create!(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: 1.minute.ago,
      alive: false
    )

    stale_hubs = Hub.stale
    assert_not_includes stale_hubs, active_hub
    assert_includes stale_hubs, stale_hub
    assert_includes stale_hubs, dead_hub
  end

  test "active? returns true for alive hub seen within 2 minutes" do
    hub = Hub.new(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: 1.minute.ago,
      alive: true
    )
    assert hub.active?
  end

  test "active? returns false for hub not seen within 2 minutes" do
    hub = Hub.new(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: 5.minutes.ago,
      alive: true
    )
    assert_not hub.active?
  end

  test "active? returns false for dead hub even if recently seen" do
    hub = Hub.new(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: 1.minute.ago,
      alive: false
    )
    assert_not hub.active?
  end

  test "active? returns true for alive hub seen just now" do
    hub = Hub.new(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current,
      alive: true
    )
    assert hub.active?
  end

  test "active? returns false for hub seen exactly 2 minutes ago" do
    hub = Hub.new(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: 2.minutes.ago,
      alive: true
    )
    assert_not hub.active?
  end

  test "name returns column value when set" do
    hub = Hub.new(
      user: @user,
      name: "My Custom Name",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    assert_equal "My Custom Name", hub.name
  end

  test "name returns column value over device name" do
    device = @user.devices.create!(
      name: "My CLI",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    hub = Hub.new(
      user: @user,
      name: "Custom Name",
      device: device,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    assert_equal "Custom Name", hub.name
  end

  test "name returns device name when column is blank" do
    device = @user.devices.create!(
      name: "My CLI",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    hub = Hub.new(
      user: @user,
      device: device,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    assert_equal "My CLI", hub.name
  end

  test "name returns truncated identifier when no column and no device" do
    hub = Hub.new(
      user: @user,
      identifier: "a-very-long-hub-identifier-string",
      last_seen_at: Time.current
    )
    assert_equal "a-very-long-hub-i...", hub.name
  end

  test "e2e_enabled? returns true when device is present" do
    device = @user.devices.create!(
      name: "CLI",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    hub = Hub.new(
      user: @user,
      device: device,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    assert hub.e2e_enabled?
  end

  test "e2e_enabled? returns false when no device" do
    hub = Hub.new(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    assert_not hub.e2e_enabled?
  end

  test "with_device scope returns only hubs with a device" do
    device = @user.devices.create!(
      name: "CLI",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8)
    )
    hub_with_device = Hub.create!(
      user: @user,
      device: device,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    hub_without_device = Hub.create!(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )

    result = Hub.with_device
    assert_includes result, hub_with_device
    assert_not_includes result, hub_without_device
  end

  test "next_message_sequence! increments atomically" do
    hub = Hub.create!(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )

    seq1 = hub.next_message_sequence!
    seq2 = hub.next_message_sequence!
    seq3 = hub.next_message_sequence!

    assert_equal 1, seq1
    assert_equal 2, seq2
    assert_equal 3, seq3
  end

  # ==========================================================================
  # Turbo Stream Broadcasts
  # ==========================================================================

  test "creating hub broadcasts hubs list update" do
    assert_turbo_stream_broadcasts [ @user, :hubs ] do
      Hub.create!(user: @user, identifier: SecureRandom.uuid, last_seen_at: Time.current)
    end
  end

  test "updating hub broadcasts hubs list update" do
    hub = Hub.create!(user: @user, identifier: SecureRandom.uuid, last_seen_at: 1.minute.ago, alive: true)

    assert_turbo_stream_broadcasts [ @user, :hubs ] do
      hub.update!(last_seen_at: Time.current)
    end
  end

  test "destroying hub broadcasts hubs list update" do
    hub = Hub.create!(user: @user, identifier: SecureRandom.uuid, last_seen_at: Time.current)

    assert_turbo_stream_broadcasts [ @user, :hubs ] do
      hub.destroy!
    end
  end

  test "hubs list broadcast targets sidebar and dashboard" do
    streams = capture_turbo_stream_broadcasts [ @user, :hubs ] do
      Hub.create!(user: @user, identifier: SecureRandom.uuid, last_seen_at: Time.current)
    end

    update_streams = streams.select { |s| s["action"] == "update" }
    targets = update_streams.map { |s| s["targets"] }
    assert_includes targets, ".hubs-list", "Expected sidebar broadcast"
    assert_includes targets, ".hubs-dashboard", "Expected dashboard broadcast"
  end

  test "destroying hub broadcasts health offline" do
    hub = Hub.create!(user: @user, identifier: SecureRandom.uuid, last_seen_at: Time.current, alive: true)
    health_stream = "hub:#{hub.id}:health"

    assert_broadcast_on(health_stream, { type: "health", cli: "offline" }) do
      hub.destroy!
    end
  end

  test "marking hub offline broadcasts health status change" do
    hub = Hub.create!(user: @user, identifier: SecureRandom.uuid, last_seen_at: Time.current, alive: true)
    health_stream = "hub:#{hub.id}:health"

    assert_broadcast_on(health_stream, { type: "health", cli: "offline" }) do
      hub.update!(alive: false)
    end
  end

  test "heartbeat-only update does not broadcast health when status unchanged" do
    hub = Hub.create!(user: @user, identifier: SecureRandom.uuid, last_seen_at: 30.seconds.ago, alive: true)
    health_stream = "hub:#{hub.id}:health"

    assert_no_broadcasts(health_stream) do
      hub.update!(last_seen_at: Time.current)
    end
  end
end
