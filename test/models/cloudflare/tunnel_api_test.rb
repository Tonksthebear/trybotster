# frozen_string_literal: true

require "test_helper"

class Cloudflare::TunnelApiTest < ActiveSupport::TestCase
  setup do
    @api = Cloudflare::TunnelApi.new(account_id: "acct_123", api_token: "cf_api_token", zone_id: "zone_123")
    @base = "https://api.cloudflare.com/client/v4"
  end

  test "list_tunnels sends GET account cfd_tunnel filtered by name" do
    stub_request(:get, "#{@base}/accounts/acct_123/cfd_tunnel?name=botster-hub-1")
      .with(headers: { "Authorization" => "Bearer cf_api_token" })
      .to_return_json(body: { success: true, result: [ { id: "tun_1", name: "botster-hub-1" } ] })

    assert_equal "tun_1", @api.list_tunnels(name: "botster-hub-1").first.fetch("id")
  end

  test "create_tunnel sends config_src cloudflare" do
    stub = stub_request(:post, "#{@base}/accounts/acct_123/cfd_tunnel")
      .with(body: { name: "botster-hub-1", tunnel_secret: "secret", config_src: "cloudflare" }.to_json)
      .to_return_json(body: { success: true, result: { id: "tun_1", name: "botster-hub-1" } })

    assert_equal "tun_1", @api.create_tunnel(name: "botster-hub-1", tunnel_secret: "secret").fetch("id")
    assert_requested stub
  end

  test "update_tunnel sends patch with tunnel_secret for rotation" do
    stub = stub_request(:patch, "#{@base}/accounts/acct_123/cfd_tunnel/tun_1")
      .with(body: { name: "botster-hub-1", tunnel_secret: "new-secret" }.to_json)
      .to_return_json(body: { success: true, result: { id: "tun_1" } })

    @api.update_tunnel(tunnel_id: "tun_1", name: "botster-hub-1", tunnel_secret: "new-secret")
    assert_requested stub
  end

  test "get_token returns raw token result" do
    stub_request(:get, "#{@base}/accounts/acct_123/cfd_tunnel/tun_1/token")
      .to_return_json(body: { success: true, result: "connector-token" })

    assert_equal "connector-token", @api.get_token(tunnel_id: "tun_1")
  end

  test "put_configuration sends ingress array" do
    ingress = [ { hostname: "hub.example.test", service: "http://localhost:3000" }, { service: "http_status:404" } ]
    stub = stub_request(:put, "#{@base}/accounts/acct_123/cfd_tunnel/tun_1/configurations")
      .with(body: { config: { ingress: ingress } }.to_json)
      .to_return_json(body: { success: true, result: { id: "config_1" } })

    @api.put_configuration(tunnel_id: "tun_1", ingress: ingress)
    assert_requested stub
  end

  test "create_dns_record sends CNAME to tunnel target" do
    stub = stub_request(:post, "#{@base}/zones/zone_123/dns_records")
      .with(body: { type: "CNAME", name: "hub.example.test", content: "tun_1.cfargotunnel.com", proxied: true }.to_json)
      .to_return_json(body: { success: true, result: { id: "dns_1" } })

    assert_equal "dns_1", @api.create_dns_record(hostname: "hub.example.test", target: "tun_1.cfargotunnel.com").fetch("id")
    assert_requested stub
  end

  test "delete endpoints map to Cloudflare paths" do
    dns = stub_request(:delete, "#{@base}/zones/zone_123/dns_records/dns_1")
      .to_return_json(body: { success: true, result: { id: "dns_1" } })
    connections = stub_request(:delete, "#{@base}/accounts/acct_123/cfd_tunnel/tun_1/connections")
      .to_return_json(body: { success: true, result: nil })
    tunnel = stub_request(:delete, "#{@base}/accounts/acct_123/cfd_tunnel/tun_1")
      .to_return_json(body: { success: true, result: nil })

    @api.delete_dns_record(dns_record_id: "dns_1")
    @api.delete_connections(tunnel_id: "tun_1")
    @api.delete_tunnel(tunnel_id: "tun_1")

    assert_requested dns
    assert_requested connections
    assert_requested tunnel
  end

  test "maps Cloudflare 429 with retry after" do
    stub_request(:get, "#{@base}/accounts/acct_123/cfd_tunnel")
      .to_return(status: 429, headers: { "Retry-After" => "30" }, body: { errors: [ { message: "rate limited" } ] }.to_json)

    error = assert_raises(Cloudflare::TunnelApi::Error) { @api.list_tunnels(name: nil) }
    assert_equal 429, error.status
    assert_equal "30", error.retry_after
    assert_predicate error, :rate_limited?
  end

  test "maps 5xx to retryable error" do
    stub_request(:get, "#{@base}/accounts/acct_123/cfd_tunnel")
      .to_return(status: 502, body: { errors: [ { message: "bad gateway" } ] }.to_json)

    error = assert_raises(Cloudflare::TunnelApi::Error) { @api.list_tunnels(name: nil) }
    assert error.retryable
  end
end
