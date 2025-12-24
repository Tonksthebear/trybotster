# frozen_string_literal: true

require "test_helper"

class TunnelChannelTest < ActionCable::Channel::TestCase
  setup do
    @user = users(:one)
    @other_user = users(:two)

    @hub = Hub.create!(
      user: @user,
      repo: "owner/repo",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )

    @hub_agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123",
      tunnel_port: nil,
      tunnel_status: "disconnected"
    )
  end

  teardown do
    @hub&.destroy
  end

  test "subscribes to hub tunnel stream" do
    stub_connection current_user: @user

    subscribe hub_id: @hub.identifier

    assert subscription.confirmed?
    assert_has_stream "tunnel_hub_#{@hub.id}"
  end

  test "rejects subscription for non-existent hub" do
    stub_connection current_user: @user

    subscribe hub_id: "non-existent-hub"

    assert subscription.rejected?
  end

  test "rejects subscription for other user's hub" do
    stub_connection current_user: @other_user

    subscribe hub_id: @hub.identifier

    assert subscription.rejected?
  end

  test "register_agent_tunnel updates agent" do
    stub_connection current_user: @user
    subscribe hub_id: @hub.identifier

    assert_not @hub_agent.tunnel_connected?
    assert_nil @hub_agent.tunnel_port

    perform :register_agent_tunnel, { session_key: @hub_agent.session_key, port: 4001 }

    @hub_agent.reload
    assert_equal 4001, @hub_agent.tunnel_port
    assert_equal "connected", @hub_agent.tunnel_status
    assert @hub_agent.tunnel_connected?
    assert_not_nil @hub_agent.tunnel_connected_at
  end

  test "register_agent_tunnel ignores non-existent agent" do
    stub_connection current_user: @user
    subscribe hub_id: @hub.identifier

    # Should not raise an error
    perform :register_agent_tunnel, { session_key: "non-existent", port: 4001 }

    @hub_agent.reload
    assert_not @hub_agent.tunnel_connected?
  end

  test "register_agent_tunnel updates existing tunnel info" do
    stub_connection current_user: @user
    subscribe hub_id: @hub.identifier

    @hub_agent.update!(tunnel_port: 4001, tunnel_status: "connected")

    perform :register_agent_tunnel, { session_key: @hub_agent.session_key, port: 4002 }

    @hub_agent.reload
    assert_equal 4002, @hub_agent.tunnel_port
    assert_equal "connected", @hub_agent.tunnel_status
  end

  test "http_response fulfills the response store" do
    stub_connection current_user: @user
    subscribe hub_id: @hub.identifier

    request_id = SecureRandom.uuid
    response_data = {
      "request_id" => request_id,
      "status" => 200,
      "body" => "<h1>Test</h1>",
      "content_type" => "text/html"
    }

    # Set up a waiter in a thread
    result = nil
    thread = Thread.new do
      result = TunnelResponseStore.wait_for(request_id, timeout: 5)
    end

    # Give the thread time to start waiting
    sleep 0.05

    # Perform the http_response action
    perform :http_response, response_data

    thread.join

    # The result will include the "action" key added by ActionCable
    assert_not_nil result
    assert_equal 200, result["status"]
    assert_equal "<h1>Test</h1>", result["body"]
    assert_equal "text/html", result["content_type"]
    assert_equal request_id, result["request_id"]
  end

  test "unsubscribed marks all agents as disconnected" do
    stub_connection current_user: @user
    subscribe hub_id: @hub.identifier

    # Set up multiple connected agents
    @hub_agent.update!(tunnel_status: "connected")
    agent2 = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-456",
      tunnel_status: "connected"
    )

    assert @hub_agent.tunnel_connected?
    assert agent2.tunnel_connected?

    # Unsubscribe
    unsubscribe

    @hub_agent.reload
    agent2.reload

    assert_not @hub_agent.tunnel_connected?
    assert_not agent2.tunnel_connected?
    assert_equal "disconnected", @hub_agent.tunnel_status
    assert_equal "disconnected", agent2.tunnel_status
  end

  test "unsubscribed only affects connected agents" do
    stub_connection current_user: @user
    subscribe hub_id: @hub.identifier

    # One connected, one already disconnected
    @hub_agent.update!(tunnel_status: "connected")
    disconnected_agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-456",
      tunnel_status: "disconnected"
    )

    unsubscribe

    @hub_agent.reload
    disconnected_agent.reload

    assert_equal "disconnected", @hub_agent.tunnel_status
    assert_equal "disconnected", disconnected_agent.tunnel_status
  end
end
