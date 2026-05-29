# frozen_string_literal: true

require "test_helper"

class Hubs::StableWebhookHostnamesControllerTest < ActionDispatch::IntegrationTest
  setup do
    @hub = hubs(:hub_without_fingerprint)
    @hub_token = @hub.create_hub_token!(name: "Stable Hostname Test Token")
  end

  test "POST requires hub bearer token" do
    post hub_stable_webhook_hostnames_path(@hub), params: hostname_params, as: :json

    assert_response :unauthorized
  end

  test "POST allocates unique hostname and updates tunnel ingress" do
    with_cloudflare_credentials do
      stub_tunnel_create_flow
      dns = stub_request(:post, cloudflare_url("/zones/zone_123/dns_records"))
        .to_return_json(body: { success: true, result: { id: "dns-created" } })
      config = stub_request(:put, %r{\A#{Regexp.escape(cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-created/configurations"))}\z})
        .to_return_json(body: { success: true, result: {} })

      post hub_stable_webhook_hostnames_path(@hub), headers: auth_headers, params: hostname_params, as: :json

      assert_response :created
      json = JSON.parse(response.body).fetch("stable_webhook_hostname")
      assert_match(/\.example\.test\z/, json.fetch("hostname"))
      assert_equal "https://#{json.fetch("hostname")}", json.fetch("public_url")
      assert_requested dns
      assert_requested config, times: 2
      refute_includes response.body, "connector-token-created"
    end
  end

  test "POST reuses active owner claim" do
    tunnel = Hubs::CloudflareTunnel.create!(
      hub: @hub,
      cloudflare_tunnel_id: "cf-existing",
      cloudflare_tunnel_name: Hubs::CloudflareTunnel.stable_name_for(@hub),
      status: "active"
    )
    existing = Hubs::StableWebhookHostname.create!(
      hub: @hub,
      cloudflare_tunnel: tunnel,
      hostname: "existing.example.test",
      public_url: "https://existing.example.test",
      owner_plugin: "github",
      owner_key: "repo",
      local_service_url: "http://localhost:3000",
      status: "active"
    )

    post hub_stable_webhook_hostnames_path(@hub), headers: auth_headers, params: hostname_params, as: :json

    assert_response :created
    assert_equal existing.hostname, JSON.parse(response.body).fetch("stable_webhook_hostname").fetch("hostname")
  end

  test "POST maps Cloudflare DNS conflict without leaking credentials" do
    with_cloudflare_credentials do
      stub_tunnel_create_flow
      stub_request(:post, cloudflare_url("/zones/zone_123/dns_records"))
        .to_return(status: 409, body: { errors: [ { message: "conflict cf_api_token" } ] }.to_json)

      post hub_stable_webhook_hostnames_path(@hub), headers: auth_headers, params: hostname_params, as: :json

      assert_response :bad_gateway
      refute_includes response.body, "cf_api_token"
    end
  end

  test "DELETE removes desired-state hostname and deletes DNS record" do
    tunnel = Hubs::CloudflareTunnel.create!(
      hub: @hub,
      cloudflare_tunnel_id: "cf-existing",
      cloudflare_tunnel_name: Hubs::CloudflareTunnel.stable_name_for(@hub),
      status: "active"
    )
    hostname = Hubs::StableWebhookHostname.create!(
      hub: @hub,
      cloudflare_tunnel: tunnel,
      hostname: "delete-host.example.test",
      public_url: "https://delete-host.example.test",
      dns_record_id: "dns-delete",
      local_service_url: "http://localhost:3000",
      status: "active"
    )

    with_cloudflare_credentials do
      dns = stub_request(:delete, cloudflare_url("/zones/zone_123/dns_records/dns-delete"))
        .to_return_json(body: { success: true, result: nil })
      config = stub_request(:put, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-existing/configurations"))
        .to_return_json(body: { success: true, result: {} })

      delete hub_stable_webhook_hostname_path(@hub, hostname), headers: auth_headers, as: :json

      assert_response :no_content
      assert_predicate hostname.reload, :revoked?
      assert_requested dns
      assert_requested config
    end
  end

  test "DELETE rejects hostname owned by another hub" do
    foreign = hubs(:active_hub)
    tunnel = hubs_cloudflare_tunnels(:active)
    hostname = Hubs::StableWebhookHostname.create!(
      hub: foreign,
      cloudflare_tunnel: tunnel,
      hostname: "foreign.example.test",
      public_url: "https://foreign.example.test",
      local_service_url: "http://localhost:3000",
      status: "active"
    )

    delete hub_stable_webhook_hostname_path(@hub, hostname), headers: auth_headers, as: :json

    assert_response :not_found
  end

  private

  def hostname_params
    {
      owner_plugin: "github",
      owner_key: "repo",
      purpose: "webhook",
      local_service_url: "http://localhost:3000"
    }
  end

  def auth_headers
    {
      "Authorization" => "Bearer #{@hub_token.token}",
      "Content-Type" => "application/json",
      "Accept" => "application/json"
    }
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

  def stub_tunnel_create_flow
    tunnel_name = Hubs::CloudflareTunnel.stable_name_for(@hub)
    stub_request(:get, cloudflare_url("/accounts/acct_123/cfd_tunnel?name=#{tunnel_name}"))
      .to_return_json(body: { success: true, result: [] })
    stub_request(:post, cloudflare_url("/accounts/acct_123/cfd_tunnel"))
      .to_return_json(body: { success: true, result: { id: "cf-created", name: tunnel_name } })
    stub_request(:get, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-created/token"))
      .to_return_json(body: { success: true, result: "connector-token-created" })
    stub_request(:put, cloudflare_url("/accounts/acct_123/cfd_tunnel/cf-created/configurations"))
      .to_return_json(body: { success: true, result: {} })
  end
end
