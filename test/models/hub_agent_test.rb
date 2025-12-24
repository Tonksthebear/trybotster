# frozen_string_literal: true

require "test_helper"

class HubAgentTest < ActiveSupport::TestCase
  setup do
    @user = users(:one)
    @hub = Hub.create!(
      user: @user,
      repo: "owner/repo",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )
  end

  teardown do
    @hub&.destroy
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

    HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123"
    )
    agent2 = HubAgent.new(
      hub: other_hub,
      session_key: "owner-repo-123"
    )
    assert agent2.valid?

    other_hub.destroy
  end

  test "delegates user to hub" do
    agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123"
    )
    assert_equal @user, agent.user
  end

  # Tunnel functionality tests
  test "tunnel_status defaults to disconnected" do
    agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123"
    )
    assert_equal "disconnected", agent.tunnel_status
    assert_not agent.tunnel_connected?
  end

  test "mark_tunnel_connected! updates status and timestamp" do
    agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123"
    )

    agent.mark_tunnel_connected!

    assert_equal "connected", agent.tunnel_status
    assert agent.tunnel_connected?
    assert_not_nil agent.tunnel_connected_at
  end

  test "mark_tunnel_disconnected! updates status" do
    agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123",
      tunnel_status: "connected",
      tunnel_connected_at: Time.current
    )

    agent.mark_tunnel_disconnected!

    assert_equal "disconnected", agent.tunnel_status
    assert_not agent.tunnel_connected?
  end

  test "tunnel_port validation allows valid ports" do
    agent = HubAgent.new(
      hub: @hub,
      session_key: "owner-repo-123",
      tunnel_port: 3000
    )
    assert agent.valid?
  end

  test "tunnel_port validation rejects invalid ports" do
    agent = HubAgent.new(
      hub: @hub,
      session_key: "owner-repo-123",
      tunnel_port: 0
    )
    assert_not agent.valid?

    agent.tunnel_port = 70000
    assert_not agent.valid?
  end

  test "tunnel_port allows nil" do
    agent = HubAgent.new(
      hub: @hub,
      session_key: "owner-repo-123",
      tunnel_port: nil
    )
    assert agent.valid?
  end

  # Sharing functionality tests
  test "enable_sharing! generates token and enables sharing" do
    agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123"
    )

    assert_not agent.sharing_enabled?

    agent.enable_sharing!

    assert agent.sharing_enabled?
    assert_not_nil agent.tunnel_share_token
    assert agent.tunnel_share_enabled
  end

  test "disable_sharing! clears token and disables sharing" do
    agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123",
      tunnel_share_token: "abc123",
      tunnel_share_enabled: true
    )

    assert agent.sharing_enabled?

    agent.disable_sharing!

    assert_not agent.sharing_enabled?
    assert_nil agent.tunnel_share_token
    assert_not agent.tunnel_share_enabled
  end

  test "sharing_enabled? returns false when token missing" do
    agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123",
      tunnel_share_enabled: true,
      tunnel_share_token: nil
    )
    assert_not agent.sharing_enabled?
  end

  test "sharing_enabled? returns false when not enabled" do
    agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123",
      tunnel_share_enabled: false,
      tunnel_share_token: "abc123"
    )
    assert_not agent.sharing_enabled?
  end

  # Scopes tests
  test "with_tunnel scope returns agents with tunnel_port" do
    agent_with_tunnel = HubAgent.create!(
      hub: @hub,
      session_key: "with-tunnel",
      tunnel_port: 4001
    )
    agent_without_tunnel = HubAgent.create!(
      hub: @hub,
      session_key: "without-tunnel",
      tunnel_port: nil
    )

    result = HubAgent.with_tunnel
    assert_includes result, agent_with_tunnel
    assert_not_includes result, agent_without_tunnel
  end

  test "tunnel_connected scope returns connected agents" do
    connected_agent = HubAgent.create!(
      hub: @hub,
      session_key: "connected",
      tunnel_status: "connected"
    )
    disconnected_agent = HubAgent.create!(
      hub: @hub,
      session_key: "disconnected",
      tunnel_status: "disconnected"
    )

    result = HubAgent.tunnel_connected
    assert_includes result, connected_agent
    assert_not_includes result, disconnected_agent
  end

  test "shared scope returns agents with sharing enabled" do
    shared_agent = HubAgent.create!(
      hub: @hub,
      session_key: "shared",
      tunnel_share_enabled: true,
      tunnel_share_token: "abc123"
    )
    not_shared_agent = HubAgent.create!(
      hub: @hub,
      session_key: "not-shared",
      tunnel_share_enabled: false
    )

    result = HubAgent.shared
    assert_includes result, shared_agent
    assert_not_includes result, not_shared_agent
  end

  test "tunnel_share_token uniqueness" do
    HubAgent.create!(
      hub: @hub,
      session_key: "agent1",
      tunnel_share_token: "unique-token"
    )

    other_hub = Hub.create!(
      user: @user,
      repo: "owner/other",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )

    agent2 = HubAgent.new(
      hub: other_hub,
      session_key: "agent2",
      tunnel_share_token: "unique-token"
    )
    assert_not agent2.valid?
    assert_includes agent2.errors[:tunnel_share_token], "has already been taken"

    other_hub.destroy
  end
end
