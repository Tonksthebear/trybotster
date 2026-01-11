# frozen_string_literal: true

# HeadscaleClient - API client for Headscale coordination server
#
# Manages user namespaces and pre-auth keys for the Tailscale mesh.
# Each user gets their own isolated namespace (tailnet).
#
# API Documentation: https://headscale.net/ref/api/
class HeadscaleClient
  class Error < StandardError; end
  class AuthenticationError < Error; end
  class NotFoundError < Error; end

  def initialize(base_url: nil, api_key: nil)
    @base_url = base_url || ENV.fetch("HEADSCALE_URL", "http://localhost:8080")
    @api_key = api_key || Rails.application.credentials.dig(:headscale, :api_key) || ENV["HEADSCALE_API_KEY"]
  end

  # Create a new user (namespace) in Headscale
  #
  # @param name [String] Unique namespace name (e.g., "user-123")
  # @return [Hash] Created user data
  def create_user(name)
    response = post("/api/v1/user", { name: name })
    response["user"]
  end

  # Get a user by name
  #
  # @param name [String] Namespace name
  # @return [Hash, nil] User data or nil if not found
  def get_user(name)
    response = get("/api/v1/user", { name: name })
    response["users"]&.first
  rescue NotFoundError
    nil
  end

  # Delete a user (namespace)
  #
  # @param name [String] Namespace name
  # @return [Boolean] Success
  def delete_user(name)
    delete("/api/v1/user/#{name}")
    true
  rescue NotFoundError
    false
  end

  # Create a pre-authentication key for joining the tailnet
  #
  # @param user [String] Namespace name
  # @param reusable [Boolean] Can the key be used multiple times?
  # @param ephemeral [Boolean] Will nodes using this key be ephemeral?
  # @param expiration [Time] When the key expires
  # @param tags [Array<String>] ACL tags for nodes using this key
  # @return [String] The pre-auth key
  def create_preauth_key(user:, reusable: false, ephemeral: false, expiration: 1.hour.from_now, tags: [])
    response = post("/api/v1/preauthkey", {
      user: user,
      reusable: reusable,
      ephemeral: ephemeral,
      expiration: expiration.iso8601,
      aclTags: tags
    })
    response.dig("preAuthKey", "key")
  end

  # List all pre-auth keys for a user
  #
  # @param user [String] Namespace name
  # @return [Array<Hash>] List of pre-auth keys
  def list_preauth_keys(user:)
    response = get("/api/v1/preauthkey", { user: user })
    response["preAuthKeys"] || []
  end

  # Expire a pre-auth key
  #
  # @param user [String] Namespace name
  # @param key [String] The pre-auth key to expire
  # @return [Boolean] Success
  def expire_preauth_key(user:, key:)
    post("/api/v1/preauthkey/expire", { user: user, key: key })
    true
  end

  # List all nodes in a namespace
  #
  # @param user [String] Namespace name (optional, lists all if nil)
  # @return [Array<Hash>] List of nodes
  def list_nodes(user: nil)
    params = user ? { user: user } : {}
    response = get("/api/v1/node", params)
    response["nodes"] || []
  end

  # Delete a node
  #
  # @param node_id [Integer] Node ID
  # @return [Boolean] Success
  def delete_node(node_id)
    delete("/api/v1/node/#{node_id}")
    true
  rescue NotFoundError
    false
  end

  # Check if Headscale is healthy
  #
  # @return [Boolean] True if healthy
  def healthy?
    get("/health")
    true
  rescue StandardError
    false
  end

  private

  def get(path, params = {})
    uri = URI.join(@base_url, path)
    uri.query = URI.encode_www_form(params) if params.any?

    request = Net::HTTP::Get.new(uri)
    execute_request(uri, request)
  end

  def post(path, body)
    uri = URI.join(@base_url, path)
    request = Net::HTTP::Post.new(uri)
    request.body = body.to_json
    request["Content-Type"] = "application/json"
    execute_request(uri, request)
  end

  def delete(path)
    uri = URI.join(@base_url, path)
    request = Net::HTTP::Delete.new(uri)
    execute_request(uri, request)
  end

  def execute_request(uri, request)
    request["Authorization"] = "Bearer #{@api_key}" if @api_key

    http = Net::HTTP.new(uri.host, uri.port)
    http.use_ssl = uri.scheme == "https"
    http.open_timeout = 5
    http.read_timeout = 10

    response = http.request(request)

    case response.code.to_i
    when 200..299
      response.body.present? ? JSON.parse(response.body) : {}
    when 401, 403
      raise AuthenticationError, "Headscale authentication failed: #{response.body}"
    when 404
      raise NotFoundError, "Resource not found: #{uri.path}"
    else
      raise Error, "Headscale API error (#{response.code}): #{response.body}"
    end
  end
end
