# frozen_string_literal: true

require "test_helper"

class SharedTunnelsControllerTest < ActionDispatch::IntegrationTest
  setup do
    @user = users(:one)

    @hub = Hub.create!(
      user: @user,
      repo: "owner/repo",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )

    @hub_agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123",
      tunnel_port: 4001,
      tunnel_status: "connected",
      tunnel_share_enabled: true,
      tunnel_share_token: "test-share-token-abc123"
    )
  end

  teardown do
    @hub&.destroy
  end

  test "does not require authentication" do
    response_data = { "status" => 200, "body" => "Public", "content_type" => "text/html" }

    MockHelper.mock_tunnel_response_store(response_data) do
      get shared_tunnel_url(token: @hub_agent.tunnel_share_token)
    end

    assert_response :ok
  end

  test "returns not found for invalid token" do
    get shared_tunnel_url(token: "invalid-token")

    assert_response :bad_gateway
    assert_includes response.body, "Invalid or expired share link"
  end

  test "returns not found when sharing is disabled" do
    @hub_agent.update!(tunnel_share_enabled: false)

    get shared_tunnel_url(token: @hub_agent.tunnel_share_token)

    assert_response :bad_gateway
    assert_includes response.body, "Sharing disabled"
  end

  test "returns not found when sharing enabled but token is nil" do
    @hub_agent.update!(tunnel_share_token: nil)

    get shared_tunnel_url(token: "null")

    assert_response :bad_gateway
    assert_includes response.body, "Invalid or expired share link"
  end

  test "returns not found when tunnel not connected" do
    @hub_agent.update!(tunnel_status: "disconnected")

    get shared_tunnel_url(token: @hub_agent.tunnel_share_token)

    assert_response :bad_gateway
    assert_includes response.body, "Tunnel not connected"
  end

  test "proxies request when sharing enabled and tunnel connected" do
    response_data = { "status" => 200, "body" => "<h1>Shared View</h1>", "content_type" => "text/html" }

    MockHelper.mock_tunnel_response_store(response_data) do
      get "/share/#{@hub_agent.tunnel_share_token}/posts"
    end

    assert_response :ok
    # HTML responses get a base tag injected for proper asset URL resolution
    assert_includes response.body, "<h1>Shared View</h1>"
    assert_includes response.body, "<base href="
  end

  test "returns gateway timeout when tunnel doesn't respond" do
    MockHelper.mock_tunnel_response_store(nil) do
      get "/share/#{@hub_agent.tunnel_share_token}/posts"
    end

    assert_response :gateway_timeout
    assert_includes response.body, "Tunnel timeout"
  end

  test "supports POST requests" do
    response_data = { "status" => 201, "body" => '{"id":1}', "content_type" => "application/json" }

    MockHelper.mock_tunnel_response_store(response_data) do
      post "/share/#{@hub_agent.tunnel_share_token}/posts",
           params: { title: "Test" }
    end

    assert_response :created
  end

  test "supports PUT requests" do
    response_data = { "status" => 200, "body" => '{"id":1}', "content_type" => "application/json" }

    MockHelper.mock_tunnel_response_store(response_data) do
      put "/share/#{@hub_agent.tunnel_share_token}/posts/1",
          params: { title: "Updated" }
    end

    assert_response :ok
  end

  test "supports DELETE requests" do
    response_data = { "status" => 204, "body" => "", "content_type" => "text/plain" }

    MockHelper.mock_tunnel_response_store(response_data) do
      delete "/share/#{@hub_agent.tunnel_share_token}/posts/1"
    end

    assert_response :no_content
  end

  test "updates tunnel_last_request_at on successful proxy" do
    response_data = { "status" => 200, "body" => "OK", "content_type" => "text/plain" }

    @hub_agent.update!(tunnel_last_request_at: nil)

    MockHelper.mock_tunnel_response_store(response_data) do
      get shared_tunnel_url(token: @hub_agent.tunnel_share_token)
    end

    @hub_agent.reload
    assert_not_nil @hub_agent.tunnel_last_request_at
  end

  test "preserves response headers from tunnel" do
    response_data = {
      "status" => 200,
      "body" => "OK",
      "content_type" => "text/plain",
      "headers" => {
        "X-Custom-Header" => "shared-value"
      }
    }

    MockHelper.mock_tunnel_response_store(response_data) do
      get shared_tunnel_url(token: @hub_agent.tunnel_share_token)
    end

    assert_response :ok
    assert_equal "shared-value", response.headers["X-Custom-Header"]
  end

  test "works with nested paths" do
    response_data = { "status" => 200, "body" => "Nested", "content_type" => "text/html" }

    MockHelper.mock_tunnel_response_store(response_data) do
      get "/share/#{@hub_agent.tunnel_share_token}/api/v1/users/123/posts"
    end

    assert_response :ok
    # HTML responses get a base tag injected for proper asset URL resolution
    assert_includes response.body, "Nested"
  end

  test "rewrites absolute URLs in HTML to use share proxy path" do
    html_with_absolute_urls = '<link href="/assets/app.css"><a href="/page">'
    response_data = { "status" => 200, "body" => html_with_absolute_urls, "content_type" => "text/html" }

    MockHelper.mock_tunnel_response_store(response_data) do
      get shared_tunnel_url(token: @hub_agent.tunnel_share_token)
    end

    assert_response :ok
    assert_includes response.body, "href=\"/share/#{@hub_agent.tunnel_share_token}/assets/app.css\""
    assert_includes response.body, "href=\"/share/#{@hub_agent.tunnel_share_token}/page\""
  end
end
