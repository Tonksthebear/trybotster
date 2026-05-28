# frozen_string_literal: true

require "test_helper"

class Hubs::CloudflareTunnelTest < ActiveSupport::TestCase
  setup do
    @hub = hubs(:hub_without_fingerprint)
  end

  test "encrypts token_secret" do
    tunnel = create_tunnel!(token_secret: "raw-connector-token")

    raw = ActiveRecord::Base.connection.select_value(
      "select token_secret from hubs_cloudflare_tunnels where id = #{tunnel.id}"
    )
    assert_not_equal "raw-connector-token", raw
    assert_equal "raw-connector-token", tunnel.reload.token_secret
  end

  test "serializes without secrets" do
    tunnel = create_tunnel!(token_secret: "connector-token", tunnel_secret: "tunnel-secret")

    json = tunnel.as_json.stringify_keys
    refute_includes json.keys, "token_secret"
    refute_includes json.keys, "tunnel_secret"
    refute_includes json.values, "connector-token"
    refute_includes json.values, "tunnel-secret"
  end

  test "enforces one tunnel per hub" do
    create_tunnel!

    duplicate = Hubs::CloudflareTunnel.new(
      hub: @hub,
      cloudflare_tunnel_id: "cf-duplicate",
      cloudflare_tunnel_name: "botster-hub-duplicate"
    )

    assert_raises(ActiveRecord::RecordNotUnique) { duplicate.save!(validate: false) }
  end

  test "ensure_remote creates remote tunnel and records token" do
    api = FakeTunnelApi.new

    tunnel = Hubs::CloudflareTunnel.ensure_for!(@hub, api: api)

    assert_predicate tunnel, :active?
    assert_equal "cf-created", tunnel.cloudflare_tunnel_id
    assert_equal "connector-token-1", tunnel.token_secret
    assert_equal 1, tunnel.token_version
    assert_includes api.calls, :create_tunnel
    assert_includes api.calls, :put_configuration
  end

  test "ensure_remote adopts single remote tunnel by stable name" do
    api = FakeTunnelApi.new(remote_tunnels: [ { "id" => "cf-existing", "name" => Hubs::CloudflareTunnel.stable_name_for(@hub) } ])

    tunnel = Hubs::CloudflareTunnel.ensure_for!(@hub, api: api)

    assert_equal "cf-existing", tunnel.cloudflare_tunnel_id
    refute_includes api.calls, :create_tunnel
  end

  test "ensure_remote raises when multiple remote tunnels match stable name" do
    api = FakeTunnelApi.new(remote_tunnels: [
      { "id" => "cf-existing-1", "name" => Hubs::CloudflareTunnel.stable_name_for(@hub) },
      { "id" => "cf-existing-2", "name" => Hubs::CloudflareTunnel.stable_name_for(@hub) }
    ])

    assert_raises(ActiveRecord::RecordNotUnique) do
      Hubs::CloudflareTunnel.ensure_for!(@hub, api: api)
    end
  end

  test "ensure_remote records failed status on Cloudflare error" do
    tunnel = create_tunnel!
    api = FakeTunnelApi.new(error: Cloudflare::TunnelApi::Error.new("nope", status: 502, retryable: true))

    assert_raises(Cloudflare::TunnelApi::Error) { tunnel.ensure_remote!(api: api) }
    assert_predicate tunnel.reload, :failed?
    assert_equal "Cloudflare API request failed status=502", tunnel.last_error
  end

  test "rotate increments token_version under lock" do
    tunnel = create_tunnel!(cloudflare_tunnel_id: "cf-existing", token_secret: "old-token", token_version: 1)
    api = FakeTunnelApi.new(token: "new-token")

    tunnel.rotate!(api: api)

    assert_predicate tunnel.reload, :rotated?
    assert_equal 2, tunnel.token_version
    assert_equal "new-token", tunnel.token_secret
    assert_includes api.calls, :update_tunnel
  end

  test "revoke clears encrypted token and marks revoked" do
    tunnel = create_tunnel!(cloudflare_tunnel_id: "cf-existing", token_secret: "old-token", token_version: 1)
    api = FakeTunnelApi.new

    tunnel.revoke!(api: api)

    assert_predicate tunnel.reload, :revoked?
    assert_nil tunnel.token_secret
    assert_includes api.calls, :delete_connections
    assert_includes api.calls, :delete_tunnel
  end

  private

  def create_tunnel!(attributes = {})
    Hubs::CloudflareTunnel.create!({
      hub: @hub,
      cloudflare_tunnel_id: "cf-#{SecureRandom.hex(4)}",
      cloudflare_tunnel_name: Hubs::CloudflareTunnel.stable_name_for(@hub),
      status: "active"
    }.merge(attributes))
  end

  class FakeTunnelApi
    attr_reader :calls

    def initialize(remote_tunnels: [], token: "connector-token-1", error: nil)
      @remote_tunnels = remote_tunnels
      @token = token
      @error = error
      @calls = []
    end

    def list_tunnels(name:)
      raise @error if @error
      @calls << :list_tunnels
      @remote_tunnels
    end

    def create_tunnel(name:, tunnel_secret:)
      @calls << :create_tunnel
      { "id" => "cf-created", "name" => name }
    end

    def update_tunnel(tunnel_id:, name:, tunnel_secret:)
      @calls << :update_tunnel
      { "id" => tunnel_id, "name" => name }
    end

    def get_token(tunnel_id:)
      @calls << :get_token
      @token
    end

    def put_configuration(tunnel_id:, ingress:)
      @calls << :put_configuration
      true
    end

    def delete_connections(tunnel_id:)
      @calls << :delete_connections
      true
    end

    def delete_tunnel(tunnel_id:)
      @calls << :delete_tunnel
      true
    end
  end
end
