# frozen_string_literal: true

require "test_helper"

class HubAgentTest < ActiveSupport::TestCase
  setup do
    @user = users(:one)
    @hub = Hub.create!(
      user: @user,
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
end
