# frozen_string_literal: true

require "test_helper"

class HubTest < ActiveSupport::TestCase
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

  test "name returns device name when device present" do
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

  test "name returns truncated identifier when no device" do
    hub = Hub.new(
      user: @user,
      identifier: "a-very-long-hub-identifier-string",
      last_seen_at: Time.current
    )
    assert_equal "a-very-long-hub-i...", hub.name
  end

  test "destroying hub destroys associated hub_agents" do
    hub = Hub.create!(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    agent = hub.hub_agents.create!(
      session_key: "owner-repo-123",
      last_invocation_url: "https://github.com/owner/repo/issues/123"
    )

    hub.destroy
    assert_raises(ActiveRecord::RecordNotFound) { agent.reload }
  end
end
