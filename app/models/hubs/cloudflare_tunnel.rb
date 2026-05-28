# frozen_string_literal: true

module Hubs
  class CloudflareTunnel < ApplicationRecord
    self.table_name = "hubs_cloudflare_tunnels"

    STATUSES = {
      pending: "pending",
      active: "active",
      rotated: "rotated",
      failed: "failed",
      revoke_failed: "revoke_failed",
      revoked: "revoked"
    }.freeze

    belongs_to :hub
    has_many :stable_webhook_hostnames,
      class_name: "Hubs::StableWebhookHostname",
      foreign_key: :hubs_cloudflare_tunnel_id,
      dependent: :destroy,
      inverse_of: :cloudflare_tunnel

    encrypts :token_secret
    encrypts :tunnel_secret

    enum :status, STATUSES, validate: true

    validates :cloudflare_tunnel_id, presence: true, uniqueness: true
    validates :cloudflare_tunnel_name, presence: true

    def self.stable_name_for(hub)
      "botster-hub-#{hub.id}"
    end

    def self.ensure_for!(hub, api: nil)
      # The pending id only satisfies NOT NULL before the remote tunnel id is known.
      existing = hub.cloudflare_tunnel || hub.build_cloudflare_tunnel(
        cloudflare_tunnel_name: stable_name_for(hub),
        cloudflare_tunnel_id: "pending-#{SecureRandom.uuid}"
      )

      existing.ensure_remote!(api: api)
      existing
    end

    def ensure_remote!(api: nil)
      api ||= self.class.api_client

      with_lock do
        self.cloudflare_tunnel_name = self.class.stable_name_for(hub)

        remote = local_remote(api) || adoptable_remote(api) || create_remote(api)
        self.cloudflare_tunnel_id = remote.fetch("id")
        self.cloudflare_tunnel_name = remote.fetch("name", cloudflare_tunnel_name)
        refresh_token!(api: api)
        update_ingress!(api: api)
        update!(status: "active", last_error: nil, last_synced_at: Time.current)
      end
    rescue Cloudflare::TunnelApi::Error => e
      record_failure!(e)
      raise
    end

    def rotate!(api: nil)
      api ||= self.class.api_client

      with_lock do
        self.tunnel_secret = self.class.generate_tunnel_secret
        api.update_tunnel(tunnel_id: cloudflare_tunnel_id, name: cloudflare_tunnel_name, tunnel_secret: tunnel_secret)
        refresh_token!(api: api, increment_version: true)
        update!(status: "rotated", last_error: nil, last_synced_at: Time.current)
      end
    rescue Cloudflare::TunnelApi::Error => e
      record_failure!(e)
      raise
    end

    def revoke!(api: nil)
      api ||= self.class.api_client

      with_lock do
        stable_webhook_hostnames.active.find_each { |hostname| hostname.release!(api: api, update_tunnel: false) }
        api.delete_connections(tunnel_id: cloudflare_tunnel_id)
        api.delete_tunnel(tunnel_id: cloudflare_tunnel_id)
        update!(
          status: "revoked",
          token_secret: nil,
          tunnel_secret: nil,
          last_error: nil,
          last_synced_at: Time.current
        )
      end
    rescue Cloudflare::TunnelApi::Error => e
      update!(status: "revoke_failed", last_error: e.message) if persisted?
      raise
    end

    def update_ingress!(api: nil, exclude_hostnames: [])
      api ||= self.class.api_client
      api.put_configuration(tunnel_id: cloudflare_tunnel_id, ingress: ingress_rules(exclude_hostnames: exclude_hostnames))
    end

    def ingress_rules(exclude_hostnames: [])
      excluded_ids = exclude_hostnames.map(&:id).compact
      hostnames = stable_webhook_hostnames.active.order(:created_at)
      hostnames = hostnames.where.not(id: excluded_ids) if excluded_ids.any?

      hostnames.map(&:ingress_rule) + [
        { service: "http_status:404" }
      ]
    end

    def public_json(include_connector_token: false)
      {
        id: id,
        hub_id: hub_id,
        cloudflare_tunnel_id: cloudflare_tunnel_id,
        cloudflare_tunnel_name: cloudflare_tunnel_name,
        token_version: token_version,
        token_delivered_at: token_delivered_at,
        token_expires_at: token_expires_at,
        status: status,
        last_error: last_error,
        last_synced_at: last_synced_at
      }.tap do |payload|
        payload[:connector_token] = token_secret if include_connector_token
      end
    end

    def as_json(*)
      public_json
    end

    def self.api_client
      credentials = Rails.application.credentials.cloudflare || {}
      new_api_client(
        account_id: credentials.fetch(:account_id),
        api_token: credentials.fetch(:api_token),
        zone_id: credentials[:zone_id]
      )
    end

    def self.new_api_client(account_id:, api_token:, zone_id: nil)
      Cloudflare::TunnelApi.new(account_id: account_id, api_token: api_token, zone_id: zone_id)
    end

    def self.zone_name
      (Rails.application.credentials.cloudflare || {}).fetch(:zone_name)
    end

    def self.hostname_prefix
      (Rails.application.credentials.cloudflare || {})[:hostname_prefix].presence || "hub"
    end

    def self.generate_tunnel_secret
      SecureRandom.base64(32)
    end

    private

    def refresh_token!(api:, increment_version: false)
      # Persisting the connector token avoids depending on Cloudflare availability for hub re-delivery.
      # It is encrypted at rest and only emitted by create/rotate hub-token responses.
      self.token_secret = api.get_token(tunnel_id: cloudflare_tunnel_id)
      self.token_version += 1 if increment_version || token_version.zero?
      self.token_delivered_at = Time.current
      self.token_expires_at = nil
    end

    def local_remote(api)
      return if cloudflare_tunnel_id.blank? || cloudflare_tunnel_id.start_with?("pending-")

      api.list_tunnels(name: cloudflare_tunnel_name).find { |tunnel| tunnel["id"] == cloudflare_tunnel_id && !deleted_remote?(tunnel) }
    end

    def adoptable_remote(api)
      matches = api.list_tunnels(name: cloudflare_tunnel_name).reject { |tunnel| deleted_remote?(tunnel) }
      raise ActiveRecord::RecordNotUnique, "multiple Cloudflare tunnels named #{cloudflare_tunnel_name}" if matches.many?

      matches.first
    end

    def create_remote(api)
      self.tunnel_secret ||= self.class.generate_tunnel_secret
      api.create_tunnel(name: cloudflare_tunnel_name, tunnel_secret: tunnel_secret)
    end

    def deleted_remote?(tunnel)
      tunnel["deleted_at"].present? || tunnel["status"] == "deleted"
    end

    def record_failure!(error)
      update!(status: "failed", last_error: non_secret_error(error)) if persisted?
    end

    def non_secret_error(error)
      status = error.status ? " status=#{error.status}" : nil
      "Cloudflare API request failed#{status}"
    end
  end
end
