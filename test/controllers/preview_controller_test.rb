# frozen_string_literal: true

require "test_helper"

class PreviewControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  # Helper to get request options with SW cookie (version hash)
  def with_sw_cookie
    version = Digest::MD5.hexdigest(
      File.read(Rails.root.join("app/views/preview/service_worker.js.erb"))
    )[0..7]
    { headers: { "Cookie" => "tunnel_sw=#{version}" } }
  end

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
      tunnel_port: 4001,
      tunnel_status: "connected"
    )
  end

  teardown do
    @hub&.destroy
  end

  test "requires authentication" do
    get tunnel_preview_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key, path: "posts")

    assert_response :redirect
  end

  test "returns not found for non-existent hub" do
    sign_in @user

    get tunnel_preview_url(hub_id: "non-existent", agent_id: @hub_agent.session_key, path: "posts")

    assert_response :bad_gateway
    assert_includes response.body, "Hub not found"
  end

  test "returns not found for non-existent agent" do
    sign_in @user

    get tunnel_preview_url(hub_id: @hub.identifier, agent_id: "non-existent", path: "posts")

    assert_response :bad_gateway
    assert_includes response.body, "Agent not found"
  end

  test "returns not found when tunnel not connected" do
    sign_in @user
    @hub_agent.update!(tunnel_status: "disconnected")

    get tunnel_preview_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key, path: "posts")

    assert_response :bad_gateway
    assert_includes response.body, "Tunnel not connected"
  end

  test "cannot access other user's hub" do
    sign_in @other_user

    get tunnel_preview_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key, path: "posts")

    assert_response :bad_gateway
    assert_includes response.body, "Hub not found"
  end

  test "shows bootstrap page when service worker not installed" do
    sign_in @user

    get tunnel_root_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key)

    assert_response :ok
    assert_includes response.body, "Initializing secure tunnel"
    assert_includes response.body, "serviceWorker"
    assert_includes response.body, "/preview/#{@hub.identifier}/#{@hub_agent.session_key}/sw.js"
  end

  test "serves service worker javascript" do
    sign_in @user

    get tunnel_service_worker_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key)

    assert_response :ok
    assert_equal "application/javascript", response.content_type
    assert_includes response.body, "PROXY_BASE"
    assert_includes response.body, "/preview/#{@hub.identifier}/#{@hub_agent.session_key}"
    assert_includes response.body, "self.addEventListener('fetch'"
  end

  test "proxies request when tunnel connected" do
    sign_in @user
    response_data = { "status" => 200, "body" => "<h1>Hello</h1>", "content_type" => "text/html" }

    MockHelper.mock_tunnel_response_store(response_data) do
      get tunnel_preview_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key, path: "posts"), **with_sw_cookie
    end

    assert_response :ok
    # HTML responses get a base tag injected for proper asset URL resolution
    assert_includes response.body, "<h1>Hello</h1>"
    assert_includes response.body, "<base href="
  end

  test "returns gateway timeout when tunnel doesn't respond" do
    sign_in @user

    MockHelper.mock_tunnel_response_store(nil) do
      get tunnel_preview_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key, path: "posts"), **with_sw_cookie
    end

    assert_response :gateway_timeout
    assert_includes response.body, "Tunnel timeout"
  end

  test "supports POST requests" do
    sign_in @user
    response_data = { "status" => 201, "body" => '{"id":1}', "content_type" => "application/json" }

    MockHelper.mock_tunnel_response_store(response_data) do
      post tunnel_preview_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key, path: "posts"),
           params: { title: "Test" }, **with_sw_cookie
    end

    assert_response :created
  end

  test "supports PUT requests" do
    sign_in @user
    response_data = { "status" => 200, "body" => '{"id":1}', "content_type" => "application/json" }

    MockHelper.mock_tunnel_response_store(response_data) do
      put tunnel_preview_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key, path: "posts/1"),
          params: { title: "Updated" }, **with_sw_cookie
    end

    assert_response :ok
  end

  test "supports DELETE requests" do
    sign_in @user
    response_data = { "status" => 204, "body" => "", "content_type" => "text/plain" }

    MockHelper.mock_tunnel_response_store(response_data) do
      delete tunnel_preview_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key, path: "posts/1"), **with_sw_cookie
    end

    assert_response :no_content
  end

  test "updates tunnel_last_request_at on successful proxy" do
    sign_in @user
    response_data = { "status" => 200, "body" => "OK", "content_type" => "text/plain" }

    @hub_agent.update!(tunnel_last_request_at: nil)

    MockHelper.mock_tunnel_response_store(response_data) do
      get tunnel_root_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key), **with_sw_cookie
    end

    @hub_agent.reload
    assert_not_nil @hub_agent.tunnel_last_request_at
  end

  test "preserves response headers from tunnel" do
    sign_in @user
    response_data = {
      "status" => 200,
      "body" => "OK",
      "content_type" => "text/plain",
      "headers" => {
        "X-Custom-Header" => "custom-value",
        "Cache-Control" => "no-cache"
      }
    }

    MockHelper.mock_tunnel_response_store(response_data) do
      get tunnel_root_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key), **with_sw_cookie
    end

    assert_response :ok
    assert_equal "custom-value", response.headers["X-Custom-Header"]
    assert_equal "no-cache", response.headers["Cache-Control"]
  end

  test "works with root path" do
    sign_in @user
    response_data = { "status" => 200, "body" => "Root", "content_type" => "text/html" }

    MockHelper.mock_tunnel_response_store(response_data) do
      get tunnel_root_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key), **with_sw_cookie
    end

    assert_response :ok
    # HTML responses get a base tag injected for proper asset URL resolution
    assert_includes response.body, "Root"
  end

  test "rewrites absolute URLs in HTML to use proxy path" do
    sign_in @user
    html_with_absolute_urls = <<~HTML
      <html>
      <head>
        <link rel="stylesheet" href="/assets/tailwind.css">
        <script src="/assets/application.js"></script>
      </head>
      <body>
        <img src="/images/logo.png">
        <a href="/about">About</a>
        <form action="/submit"></form>
        <div style="background: url(/images/bg.png)"></div>
      </body>
      </html>
    HTML
    response_data = { "status" => 200, "body" => html_with_absolute_urls, "content_type" => "text/html" }

    MockHelper.mock_tunnel_response_store(response_data) do
      get tunnel_preview_url(hub_id: @hub.identifier, agent_id: @hub_agent.session_key, path: "page"), **with_sw_cookie
    end

    assert_response :ok
    # Absolute URLs should be rewritten to include the proxy path
    assert_includes response.body, "href=\"/preview/#{@hub.identifier}/#{@hub_agent.session_key}/assets/tailwind.css\""
    assert_includes response.body, "src=\"/preview/#{@hub.identifier}/#{@hub_agent.session_key}/assets/application.js\""
    assert_includes response.body, "src=\"/preview/#{@hub.identifier}/#{@hub_agent.session_key}/images/logo.png\""
    assert_includes response.body, "href=\"/preview/#{@hub.identifier}/#{@hub_agent.session_key}/about\""
    assert_includes response.body, "action=\"/preview/#{@hub.identifier}/#{@hub_agent.session_key}/submit\""
    assert_includes response.body, "url(/preview/#{@hub.identifier}/#{@hub_agent.session_key}/images/bg.png)"
  end
end
