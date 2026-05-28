# frozen_string_literal: true

require "test_helper"

class Hubs::CloudflareTunnelsControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @hub = hubs(:hub_without_fingerprint)
    @hub_token = @hub.create_hub_token!(name: "Cloudflare Test Token")
  end

  test "GET requires hub bearer token" do
    get hub_cloudflare_tunnel_path(@hub), as: :json

    assert_response :unauthorized
  end

  test "GET returns metadata without connector token" do
    Hubs::CloudflareTunnel.create!(
      hub: @hub,
      cloudflare_tunnel_id: "cf-get",
      cloudflare_tunnel_name: Hubs::CloudflareTunnel.stable_name_for(@hub),
      token_secret: "connector-token",
      tunnel_secret: "tunnel-secret",
      status: "active"
    )

    get hub_cloudflare_tunnel_path(@hub), headers: auth_headers

    assert_response :success
    json = JSON.parse(response.body)
    tunnel = json.fetch("cloudflare_tunnel")
    refute tunnel.key?("connector_token")
    refute_includes response.body, "connector-token"
    refute_includes response.body, "tunnel-secret"
  end

  test "POST creates new tunnel through production route and returns connector token" do
    with_cloudflare_credentials do
      stubs = stub_create_flow

      post hub_cloudflare_tunnel_path(@hub), headers: auth_headers, as: :json

      assert_response :created
      json = JSON.parse(response.body).fetch("cloudflare_tunnel")
      assert_equal "connector-token-created", json.fetch("connector_token")
      assert_equal 1, json.fetch("token_version")
      assert_requested stubs.fetch(:list)
      assert_requested stubs.fetch(:create)
      assert_requested stubs.fetch(:token)
      assert_requested stubs.fetch(:config)
    end
  end

  test "POST returns existing tunnel on second call without duplicate create" do
    Hubs::CloudflareTunnel.create!(
      hub: @hub,
      cloudflare_tunnel_id: "cf-existing",
      cloudflare_tunnel_name: Hubs::CloudflareTunnel.stable_name_for(@hub),
      token_secret: "old-token",
      status: "active"
    )

    with_cloudflare_credentials do
      list = stub_request(:get, cloudflare_url("/accounts/acct_123/cfd_tunnel?name=#{Hubs::CloudflareTunnel.stable_name_for(@hub)}"))
        .to_return_json(body: { success: true, result: [ { id: "cf-existing", name: Hubs::CloudflareTunnel.stable_name_for(@hub) } ] })
      create = stub_request(:post, cloudflare_url("/accounts/acct_123/cfd_tunnel"))
      stub_request(:get, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-existing/token"))
        .to_return_json(body: { success: true, result: "refreshed-token" })
      stub_request(:put, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-existing/configurations"))
        .to_return_json(body: { success: true, result: {} })

      post hub_cloudflare_tunnel_path(@hub), headers: auth_headers, as: :json

      assert_response :created
      assert_equal 1, Hubs::CloudflareTunnel.where(hub: @hub).count
      assert_requested list
      assert_not_requested create
    end
  end

  test "POST returns conflict when multiple remote tunnels match stable name" do
    with_cloudflare_credentials do
      tunnel_name = Hubs::CloudflareTunnel.stable_name_for(@hub)
      stub_request(:get, cloudflare_url("/accounts/acct_123/cfd_tunnel?name=#{tunnel_name}"))
        .to_return_json(body: {
          success: true,
          result: [
            { id: "cf-existing-1", name: tunnel_name },
            { id: "cf-existing-2", name: tunnel_name }
          ]
        })

      post hub_cloudflare_tunnel_path(@hub), headers: auth_headers, as: :json

      assert_response :conflict
    end
  end

  test "POST rejects foreign hub token" do
    foreign_hub = hubs(:active_hub)

    post hub_cloudflare_tunnel_path(foreign_hub), headers: auth_headers, as: :json

    assert_response :unauthorized
  end

  test "PATCH rotate returns new token_version and connector token" do
    create_existing_tunnel!

    with_cloudflare_credentials do
      stub_request(:patch, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-existing"))
        .to_return_json(body: { success: true, result: { id: "cf-existing" } })
      stub_request(:get, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-existing/token"))
        .to_return_json(body: { success: true, result: "rotated-token" })

      patch rotate_hub_cloudflare_tunnel_path(@hub), headers: auth_headers, as: :json

      assert_response :success
      json = JSON.parse(response.body).fetch("cloudflare_tunnel")
      assert_equal "rotated-token", json.fetch("connector_token")
      assert_equal 2, json.fetch("token_version")
      refute_includes response.body, "cf_api_token"
    end
  end

  test "PATCH rotate logs do not contain connector token bytes" do
    create_existing_tunnel!
    stream = StringIO.new
    logger = ActiveSupport::TaggedLogging.new(ActiveSupport::Logger.new(stream))
    old_logger = Rails.logger

    with_cloudflare_credentials do
      stub_request(:patch, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-existing"))
        .to_return_json(body: { success: true, result: { id: "cf-existing" } })
      stub_request(:get, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-existing/token"))
        .to_return_json(body: { success: true, result: "rotated-token" })

      Rails.logger = logger
      patch rotate_hub_cloudflare_tunnel_path(@hub), headers: auth_headers, as: :json
    end

    refute_includes stream.string, "rotated-token"
  ensure
    Rails.logger = old_logger if old_logger
  end

  test "PATCH rotate maps Cloudflare 429 to 503 with retry_after" do
    create_existing_tunnel!

    with_cloudflare_credentials do
      stub_request(:patch, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-existing"))
        .to_return(status: 429, headers: { "Retry-After" => "60" }, body: { errors: [ { message: "rate limited" } ] }.to_json)

      patch rotate_hub_cloudflare_tunnel_path(@hub), headers: auth_headers, as: :json

      assert_response :service_unavailable
      assert_equal "60", JSON.parse(response.body).fetch("retry_after")
    end
  end

  test "DELETE revokes tunnel and cascades hostnames" do
    tunnel = create_existing_tunnel!
    Hubs::StableWebhookHostname.create!(
      hub: @hub,
      cloudflare_tunnel: tunnel,
      hostname: "delete.example.test",
      public_url: "https://delete.example.test",
      dns_record_id: "dns-delete",
      local_service_url: "http://localhost:3000",
      status: "active"
    )

    with_cloudflare_credentials do
      dns = stub_request(:delete, cloudflare_url("/zones/zone_123/dns_records/dns-delete"))
        .to_return_json(body: { success: true, result: nil })
      connections = stub_request(:delete, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-existing/connections"))
        .to_return_json(body: { success: true, result: nil })
      delete_tunnel = stub_request(:delete, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-existing"))
        .to_return_json(body: { success: true, result: nil })

      delete hub_cloudflare_tunnel_path(@hub), headers: auth_headers, as: :json

      assert_response :no_content
      assert_predicate tunnel.reload, :revoked?
      assert_requested dns
      assert_requested connections
      assert_requested delete_tunnel
    end
  end

  test "Cloudflare 5xx records failure state and returns non-secret error" do
    with_cloudflare_credentials do
      stub_request(:get, cloudflare_url("/accounts/acct_123/cfd_tunnel?name=#{Hubs::CloudflareTunnel.stable_name_for(@hub)}"))
        .to_return(status: 502, body: { errors: [ { message: "upstream failed connector-token-secret" } ] }.to_json)

      post hub_cloudflare_tunnel_path(@hub), headers: auth_headers, as: :json

      assert_response :bad_gateway
      refute_includes response.body, "connector-token-secret"
    end
  end

  test "browser session request cannot receive connector token" do
    sign_in @hub.user

    post hub_cloudflare_tunnel_path(@hub), as: :json

    assert_response :unauthorized
  end

  test "filters Cloudflare tunnel token parameter names" do
    filter = ActiveSupport::ParameterFilter.new(Rails.application.config.filter_parameters)
    params = {
      tunnel_token: "raw-token",
      cloudflare_api_token: "api-token",
      connector_token: "connector-token",
      nested: { token_secret: "secret-token" }
    }

    filtered = filter.filter(params)

    assert_equal "[FILTERED]", filtered[:tunnel_token]
    assert_equal "[FILTERED]", filtered[:cloudflare_api_token]
    assert_equal "[FILTERED]", filtered[:connector_token]
    assert_equal "[FILTERED]", filtered[:nested][:token_secret]
  end

  private

  def auth_headers
    {
      "Authorization" => "Bearer #{@hub_token.token}",
      "Content-Type" => "application/json",
      "Accept" => "application/json"
    }
  end

  def create_existing_tunnel!
    Hubs::CloudflareTunnel.create!(
      hub: @hub,
      cloudflare_tunnel_id: "cf-existing",
      cloudflare_tunnel_name: Hubs::CloudflareTunnel.stable_name_for(@hub),
      token_secret: "old-token",
      token_version: 1,
      status: "active"
    )
  end

  def with_cloudflare_credentials
    credentials = {
      account_id: "acct_123",
      api_token: "cf_api_token",
      zone_id: "zone_123",
      zone_name: "example.test",
      hostname_prefix: "hub"
    }
    Rails.application.credentials.stub(:cloudflare, credentials) { yield }
  end

  def cloudflare_url(path)
    "https://api.cloudflare.com/client/v4#{path}"
  end

  def stub_create_flow
    tunnel_name = Hubs::CloudflareTunnel.stable_name_for(@hub)
    {
      list: stub_request(:get, cloudflare_url("/accounts/acct_123/cfd_tunnel?name=#{tunnel_name}"))
        .to_return_json(body: { success: true, result: [] }),
      create: stub_request(:post, cloudflare_url("/accounts/acct_123/cfd_tunnel"))
        .with { |request| JSON.parse(request.body).fetch("config_src") == "cloudflare" }
        .to_return_json(body: { success: true, result: { id: "cf-created", name: tunnel_name } }),
      token: stub_request(:get, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-created/token"))
        .to_return_json(body: { success: true, result: "connector-token-created" }),
      config: stub_request(:put, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-created/configurations"))
        .to_return_json(body: { success: true, result: {} })
    }
  end
end
