# frozen_string_literal: true

require "net/http"
require "json"

module Cloudflare
  class TunnelApi
    API_BASE = "https://api.cloudflare.com/client/v4"

    class Error < StandardError
      attr_reader :status, :retry_after, :retryable

      def initialize(message, status: nil, retry_after: nil, retryable: false)
        super(message)
        @status = status
        @retry_after = retry_after
        @retryable = retryable
      end

      def rate_limited?
        status.to_i == 429
      end
    end

    attr_reader :account_id, :api_token, :zone_id

    def initialize(account_id:, api_token:, zone_id: nil)
      @account_id = account_id
      @api_token = api_token
      @zone_id = zone_id
    end

    def list_tunnels(name:)
      request(:get, "/accounts/#{account_id}/cfd_tunnel", query: { name: name }).fetch("result", [])
    end

    def create_tunnel(name:, tunnel_secret:)
      request(:post, "/accounts/#{account_id}/cfd_tunnel", body: {
        name: name,
        tunnel_secret: tunnel_secret,
        config_src: "cloudflare"
      }).fetch("result")
    end

    def update_tunnel(tunnel_id:, name:, tunnel_secret:)
      request(:patch, "/accounts/#{account_id}/cfd_tunnel/#{tunnel_id}", body: {
        name: name,
        tunnel_secret: tunnel_secret
      }).fetch("result")
    end

    def get_token(tunnel_id:)
      request(:get, "/accounts/#{account_id}/cfd_tunnel/#{tunnel_id}/token").fetch("result")
    end

    def put_configuration(tunnel_id:, ingress:)
      request(:put, "/accounts/#{account_id}/cfd_tunnel/#{tunnel_id}/configurations", body: {
        config: { ingress: ingress }
      }).fetch("result")
    end

    def create_dns_record(hostname:, target:)
      request(:post, "/zones/#{zone_id}/dns_records", body: {
        type: "CNAME",
        name: hostname,
        content: target,
        proxied: true
      }).fetch("result")
    end

    def delete_dns_record(dns_record_id:)
      request(:delete, "/zones/#{zone_id}/dns_records/#{dns_record_id}").fetch("result", nil)
    end

    def delete_connections(tunnel_id:)
      request(:delete, "/accounts/#{account_id}/cfd_tunnel/#{tunnel_id}/connections").fetch("result", nil)
    end

    def delete_tunnel(tunnel_id:)
      request(:delete, "/accounts/#{account_id}/cfd_tunnel/#{tunnel_id}").fetch("result", nil)
    end

    private

    def request(method, path, query: {}, body: nil)
      uri = URI("#{API_BASE}#{path}")
      uri.query = URI.encode_www_form(query.compact) if query.present?

      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.read_timeout = 10
      http.open_timeout = 5

      response = http.request(build_request(method, uri, body))
      parse_response(response)
    rescue Net::OpenTimeout, Net::ReadTimeout, SocketError => e
      raise Error.new(e.class.name, retryable: true)
    end

    def build_request(method, uri, body)
      klass = {
        get: Net::HTTP::Get,
        post: Net::HTTP::Post,
        patch: Net::HTTP::Patch,
        put: Net::HTTP::Put,
        delete: Net::HTTP::Delete
      }.fetch(method)

      request = klass.new(uri)
      request["Authorization"] = "Bearer #{api_token}"
      request["Content-Type"] = "application/json"
      request["Accept"] = "application/json"
      request.body = JSON.generate(body) if body
      request
    end

    def parse_response(response)
      envelope = response.body.present? ? JSON.parse(response.body) : {}
      return envelope if response.is_a?(Net::HTTPSuccess)

      message = Array(envelope["errors"]).filter_map { |error| error["message"] }.first || "Cloudflare API request failed"
      raise Error.new(
        message,
        status: response.code.to_i,
        retry_after: response["Retry-After"],
        retryable: response.code.to_i >= 500 || response.code.to_i == 429
      )
    rescue JSON::ParserError
      raise Error.new("Cloudflare API returned invalid JSON", status: response.code.to_i, retryable: true)
    end
  end
end
