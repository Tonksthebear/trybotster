# frozen_string_literal: true

require "test_helper"

class HubAgentTest < ActiveSupport::TestCase
  setup do
    @user = User.create!(
      email: "hub_agent_test_user@example.com",
      username: "hub_agent_test_user"
    )
    @hub = Hub.create!(
      user: @user,
      repo: "owner/repo",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
  end

  teardown do
    @user&.destroy
  end

  test "valid hub_agent" do
    agent = HubAgent.new(
      hub: @hub,
      session_key: "owner-repo-123",
      last_invocation_url: "https://github.com/owner/repo/issues/123"
    )
    assert agent.valid?
  end

  test "valid hub_agent without last_invocation_url" do
    agent = HubAgent.new(
      hub: @hub,
      session_key: "owner-repo-123"
    )
    assert agent.valid?
  end

  test "requires hub" do
    agent = HubAgent.new(
      session_key: "owner-repo-123"
    )
    assert_not agent.valid?
    assert_includes agent.errors[:hub], "must exist"
  end

  test "requires session_key" do
    agent = HubAgent.new(
      hub: @hub
    )
    assert_not agent.valid?
    assert_includes agent.errors[:session_key], "can't be blank"
  end

  test "session_key must be unique within hub" do
    HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123"
    )

    duplicate = HubAgent.new(
      hub: @hub,
      session_key: "owner-repo-123"
    )
    assert_not duplicate.valid?
    assert_includes duplicate.errors[:session_key], "has already been taken"
  end

  test "same session_key allowed in different hubs" do
    other_hub = Hub.create!(
      user: @user,
      repo: "owner/other-repo",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )

    agent1 = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123"
    )
    agent2 = HubAgent.new(
      hub: other_hub,
      session_key: "owner-repo-123"
    )
    assert agent2.valid?
  end

  test "delegates user to hub" do
    agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123"
    )
    assert_equal @user, agent.user
  end
end
