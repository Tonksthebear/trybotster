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
      repo: "owner/repo",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    assert hub.valid?
  end

  test "requires user" do
    hub = Hub.new(
      repo: "owner/repo",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    assert_not hub.valid?
    assert_includes hub.errors[:user], "must exist"
  end

  test "requires repo" do
    hub = Hub.new(
      user: @user,
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    assert_not hub.valid?
    assert_includes hub.errors[:repo], "can't be blank"
  end

  test "requires identifier" do
    hub = Hub.new(
      user: @user,
      repo: "owner/repo",
      last_seen_at: Time.current
    )
    assert_not hub.valid?
    assert_includes hub.errors[:identifier], "can't be blank"
  end

  test "requires last_seen_at" do
    hub = Hub.new(
      user: @user,
      repo: "owner/repo",
      identifier: SecureRandom.uuid
    )
    assert_not hub.valid?
    assert_includes hub.errors[:last_seen_at], "can't be blank"
  end

  test "identifier must be unique" do
    identifier = SecureRandom.uuid
    Hub.create!(
      user: @user,
      repo: "owner/repo",
      identifier: identifier,
      last_seen_at: Time.current
    )

    duplicate = Hub.new(
      user: @user,
      repo: "other/repo",
      identifier: identifier,
      last_seen_at: Time.current
    )
    assert_not duplicate.valid?
    assert_includes duplicate.errors[:identifier], "has already been taken"
  end

  test "active scope returns hubs seen within 2 minutes" do
    active_hub = Hub.create!(
      user: @user,
      repo: "owner/active",
      identifier: SecureRandom.uuid,
      last_seen_at: 1.minute.ago
    )
    stale_hub = Hub.create!(
      user: @user,
      repo: "owner/stale",
      identifier: SecureRandom.uuid,
      last_seen_at: 5.minutes.ago
    )

    active_hubs = Hub.active
    assert_includes active_hubs, active_hub
    assert_not_includes active_hubs, stale_hub
  end

  test "stale scope returns hubs not seen within 2 minutes" do
    active_hub = Hub.create!(
      user: @user,
      repo: "owner/active",
      identifier: SecureRandom.uuid,
      last_seen_at: 1.minute.ago
    )
    stale_hub = Hub.create!(
      user: @user,
      repo: "owner/stale",
      identifier: SecureRandom.uuid,
      last_seen_at: 5.minutes.ago
    )

    stale_hubs = Hub.stale
    assert_not_includes stale_hubs, active_hub
    assert_includes stale_hubs, stale_hub
  end

  test "for_repo scope filters by repo" do
    hub1 = Hub.create!(
      user: @user,
      repo: "owner/repo1",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
    hub2 = Hub.create!(
      user: @user,
      repo: "owner/repo2",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )

    result = Hub.for_repo("owner/repo1")
    assert_includes result, hub1
    assert_not_includes result, hub2
  end

  test "destroying hub destroys associated hub_agents" do
    hub = Hub.create!(
      user: @user,
      repo: "owner/repo",
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
