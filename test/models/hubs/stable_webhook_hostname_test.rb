# frozen_string_literal: true

require "test_helper"

class Hubs::StableWebhookHostnameTest < ActiveSupport::TestCase
  setup do
    @hub = hubs(:hub_without_fingerprint)
    @tunnel = Hubs::CloudflareTunnel.create!(
      hub: @hub,
      cloudflare_tunnel_id: "cf-hostname-test",
      cloudflare_tunnel_name: Hubs::CloudflareTunnel.stable_name_for(@hub),
      status: "active"
    )
  end

  test "allocates globally unique hostname and stores DNS metadata" do
    api = FakeHostnameApi.new

    Hubs::CloudflareTunnel.stub(:ensure_for!, @tunnel) do
      Hubs::CloudflareTunnel.stub(:api_client, api) do
        Hubs::CloudflareTunnel.stub(:zone_name, "example.test") do
          Hubs::CloudflareTunnel.stub(:hostname_prefix, "hub") do
            hostname = Hubs::StableWebhookHostname.allocate_for!(
              @hub,
              owner_plugin: "github",
              owner_key: "repo",
              purpose: "webhook",
              local_service_url: "http://localhost:3000"
            )

            assert_predicate hostname, :active?
            assert_match(/\Ahub-#{@hub.id}-[a-f0-9]+\.example\.test\z/, hostname.hostname)
            assert_equal "https://#{hostname.hostname}", hostname.public_url
            assert_equal "dns-created", hostname.dns_record_id
          end
        end
      end
    end
  end

  test "reuses active owner claim" do
    existing = Hubs::StableWebhookHostname.create!(
      hub: @hub,
      cloudflare_tunnel: @tunnel,
      hostname: "existing.example.test",
      public_url: "https://existing.example.test",
      owner_plugin: "github",
      owner_key: "repo",
      local_service_url: "http://localhost:3000",
      status: "active"
    )

    Hubs::CloudflareTunnel.stub(:ensure_for!, @tunnel) do
      assert_equal existing, Hubs::StableWebhookHostname.allocate_for!(
        @hub,
        owner_plugin: "github",
        owner_key: "repo",
        local_service_url: "http://localhost:3000"
      )
    end
  end

  test "release marks hostname revoked and updates ingress" do
    hostname = Hubs::StableWebhookHostname.create!(
      hub: @hub,
      cloudflare_tunnel: @tunnel,
      hostname: "release.example.test",
      public_url: "https://release.example.test",
      dns_record_id: "dns-release",
      local_service_url: "http://localhost:3000",
      status: "active"
    )
    api = FakeHostnameApi.new

    hostname.release!(api: api)

    assert_predicate hostname.reload, :revoked?
    assert_includes api.calls, :delete_dns_record
    assert_includes api.calls, :put_configuration
  end

  test "ingress row uses hostname and local_service_url" do
    hostname = Hubs::StableWebhookHostname.new(
      hostname: "row.example.test",
      local_service_url: "http://localhost:3000"
    )

    assert_equal({ hostname: "row.example.test", service: "http://localhost:3000" }, hostname.ingress_rule)
  end

  class FakeHostnameApi
    attr_reader :calls

    def initialize
      @calls = []
    end

    def create_dns_record(hostname:, target:)
      @calls << :create_dns_record
      { "id" => "dns-created" }
    end

    def delete_dns_record(dns_record_id:)
      @calls << :delete_dns_record
      true
    end

    def put_configuration(tunnel_id:, ingress:)
      @calls << :put_configuration
      true
    end
  end
end
