# frozen_string_literal: true

class Hub < ApplicationRecord
  belongs_to :user
  belongs_to :device, optional: true  # The CLI device running this hub
  has_many :hub_agents, dependent: :destroy

  # DEPRECATED - Tailscale integration (moved to deprecated/ folder)
  # encrypts :tailscale_preauth_key
  # before_create :generate_tailscale_preauth_key

  validates :repo, presence: true
  validates :identifier, presence: true, uniqueness: true
  validates :last_seen_at, presence: true

  scope :active, -> { where("last_seen_at > ?", 2.minutes.ago) }
  scope :for_repo, ->(repo) { where(repo: repo) }
  scope :stale, -> { where("last_seen_at <= ?", 2.minutes.ago) }
  scope :with_device, -> { where.not(device_id: nil) }

  # Check if this hub supports E2E encrypted terminal access
  def e2e_enabled?
    device.present?
  end

  # Check if this hub is active (seen within 2 minutes)
  def active?
    last_seen_at > 2.minutes.ago
  end

  # Synchronize hub_agents with data from CLI heartbeat.
  # Removes agents not in the list, creates/updates those present.
  # @param agents_data [Array<Hash>, ActionController::Parameters] Agent data from CLI
  def sync_agents(agents_data)
    agents_array = normalize_agents_data(agents_data)
    session_keys = agents_array.filter_map { |a| a[:session_key] || a["session_key"] }

    # Remove agents no longer reported by CLI
    hub_agents.where.not(session_key: session_keys).destroy_all

    # Create or update agents
    agents_array.each do |agent_data|
      session_key = agent_data[:session_key] || agent_data["session_key"]
      next if session_key.blank?

      agent = hub_agents.find_or_initialize_by(session_key: session_key)
      agent.last_invocation_url = agent_data[:last_invocation_url] || agent_data["last_invocation_url"]
      agent.save!
    end
  end

  # Broadcast Turbo Stream update for user dashboard
  def broadcast_update!
    Turbo::StreamsChannel.broadcast_update_to(
      turbo_stream_name,
      target: "hubs_list",
      partial: "hubs/list",
      locals: { hubs: user.hubs.active.includes(:hub_agents) }
    )
  rescue => e
    Rails.logger.warn "Failed to broadcast hub update: #{e.message}"
  end

  # Broadcast Turbo Stream update after hub removal (call before destroy)
  def broadcast_removal!
    Turbo::StreamsChannel.broadcast_update_to(
      turbo_stream_name,
      target: "hubs_list",
      partial: "hubs/list",
      locals: { hubs: user.hubs.active.includes(:hub_agents) }
    )
  rescue => e
    Rails.logger.warn "Failed to broadcast hub removal: #{e.message}"
  end

  # DEPRECATED - Tailscale Integration (moved to deprecated/ folder)
  # def tailscale_connected?
  #   tailscale_hostname.present?
  # end
  #
  # def create_browser_preauth_key
  #   user.create_tailscale_preauth_key(
  #     ephemeral: true,
  #     expiration: 1.hour.from_now,
  #     tags: ["tag:browser"]
  #   )
  # end

  private

  def normalize_agents_data(data)
    data.is_a?(ActionController::Parameters) ? data.values : Array(data)
  end

  def turbo_stream_name
    "user_#{user_id}_hubs"
  end

  # DEPRECATED - Tailscale pre-auth key generation
  # def generate_tailscale_preauth_key
  #   self.tailscale_preauth_key = user.create_tailscale_preauth_key(
  #     ephemeral: false,
  #     expiration: 1.year.from_now,
  #     tags: ["tag:cli", "tag:hub-#{identifier}"]
  #   )
  # rescue HeadscaleClient::Error => e
  #   Rails.logger.error "Failed to generate Tailscale pre-auth key for hub #{identifier}: #{e.message}"
  # end
end
