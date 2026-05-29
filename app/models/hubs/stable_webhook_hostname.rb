# frozen_string_literal: true

module Hubs
  class StableWebhookHostname < ApplicationRecord
    self.table_name = "hubs_stable_webhook_hostnames"

    STATUSES = {
      pending: "pending",
      active: "active",
      failed: "failed",
      revoke_failed: "revoke_failed",
      revoked: "revoked"
    }.freeze

    belongs_to :hub
    belongs_to :cloudflare_tunnel,
      class_name: "Hubs::CloudflareTunnel",
      foreign_key: :hubs_cloudflare_tunnel_id,
      inverse_of: :stable_webhook_hostnames

    enum :status, STATUSES, validate: true

    validates :hostname, presence: true, uniqueness: true
    validates :public_url, presence: true
    validates :local_service_url, presence: true

    def self.allocate_for!(hub, owner_plugin:, owner_key:, local_service_url:, purpose: nil, api: nil)
      existing = active.find_by(hub: hub, owner_plugin: owner_plugin, owner_key: owner_key)
      return existing if existing

      tunnel = CloudflareTunnel.ensure_for!(hub, api: api)
      api ||= CloudflareTunnel.api_client
      hostname = new(
        hub: hub,
        cloudflare_tunnel: tunnel,
        hostname: generated_hostname_for(hub),
        public_url: nil,
        owner_plugin: owner_plugin,
        owner_key: owner_key,
        purpose: purpose,
        local_service_url: local_service_url,
        status: "pending"
      )
      hostname.public_url = "https://#{hostname.hostname}"
      hostname.create_remote!(api: api)
      hostname
    end

    def create_remote!(api:)
      dns_record = api.create_dns_record(hostname: hostname, target: tunnel_dns_target)
      update!(dns_record_id: dns_record["id"], status: "active", last_error: nil)
      cloudflare_tunnel.update_ingress!(api: api)
    rescue Cloudflare::TunnelApi::Error => e
      save! if new_record?
      update!(status: "failed", last_error: non_secret_error(e))
      raise
    end

    def release!(api: nil, update_tunnel: true)
      api ||= CloudflareTunnel.api_client
      api.delete_dns_record(dns_record_id: dns_record_id) if dns_record_id.present?
      cloudflare_tunnel.update_ingress!(api: api, exclude_hostnames: [ self ]) if update_tunnel
      update!(status: "revoked", last_error: nil)
    rescue Cloudflare::TunnelApi::Error => e
      update!(status: "revoke_failed", last_error: non_secret_error(e))
      raise
    end

    def ingress_rule
      {
        hostname: hostname,
        service: local_service_url
      }
    end

    def public_json
      {
        id: id,
        hub_id: hub_id,
        cloudflare_tunnel_id: cloudflare_tunnel.cloudflare_tunnel_id,
        hostname: hostname,
        public_url: public_url,
        owner_plugin: owner_plugin,
        owner_key: owner_key,
        purpose: purpose,
        local_service_url: local_service_url,
        status: status,
        last_error: last_error
      }
    end

    def as_json(*)
      public_json
    end

    def self.generated_hostname_for(hub)
      "#{CloudflareTunnel.hostname_prefix}-#{hub.id}-#{SecureRandom.hex(4)}.#{CloudflareTunnel.zone_name}"
    end

    private

    def tunnel_dns_target
      "#{cloudflare_tunnel.cloudflare_tunnel_id}.cfargotunnel.com"
    end

    def non_secret_error(error)
      status = error.status ? " status=#{error.status}" : nil
      "Cloudflare API request failed#{status}"
    end
  end
end
