# frozen_string_literal: true

class Hub < ApplicationRecord
  belongs_to :user
  belongs_to :device, optional: true  # The CLI device running this hub
  has_many :hub_agents, dependent: :destroy
  has_many :bot_messages, class_name: "Bot::Message", dependent: :nullify

  validates :repo, presence: true
  validates :identifier, presence: true, uniqueness: true
  validates :last_seen_at, presence: true

  scope :active, -> { where(alive: true).where("last_seen_at > ?", 2.minutes.ago) }
  scope :for_repo, ->(repo) { where(repo: repo) }
  scope :stale, -> { where(alive: false).or(where("last_seen_at <= ?", 2.minutes.ago)) }
  scope :with_device, -> { where.not(device_id: nil) }

  after_update_commit :update_sidebar
  after_update_commit :broadcast_health_status, if: :health_status_changed?

  # Check if this hub supports E2E encrypted terminal access
  def e2e_enabled?
    device.present?
  end

  # Check if this hub is active (alive flag set and seen within 2 minutes)
  def active?
    alive? && last_seen_at > 2.minutes.ago
  end

  # Display name for the hub (uses repo name)
  def name
    repo
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

  # Broadcast Turbo Stream update for sidebar hubs list
  def broadcast_update!
    Turbo::StreamsChannel.broadcast_update_to(
      turbo_stream_name,
      target: "sidebar_hubs_list",
      partial: "layouts/sidebar_hubs",
      locals: { hubs: user.hubs.includes(:device).order(last_seen_at: :desc) }
    )
  rescue => e
    Rails.logger.warn "Failed to broadcast hub update: #{e.message}"
  end

  # Broadcast Turbo Stream update after hub removal (call before destroy)
  def broadcast_removal!
    Turbo::StreamsChannel.broadcast_update_to(
      turbo_stream_name,
      target: "sidebar_hubs_list",
      partial: "layouts/sidebar_hubs",
      locals: { hubs: user.hubs.includes(:device).order(last_seen_at: :desc) }
    )
  rescue => e
    Rails.logger.warn "Failed to broadcast hub removal: #{e.message}"
  end

  # Atomically increment and return the next message sequence number.
  # Uses row-level locking for safe concurrent access.
  def next_message_sequence!
    with_lock do
      increment!(:message_sequence)
      message_sequence
    end
  end

  private

  def normalize_agents_data(data)
    data.is_a?(ActionController::Parameters) ? data.values : Array(data)
  end

  def turbo_stream_name
    "user_#{user_id}_hubs"
  end

  def update_sidebar
    Turbo::StreamsChannel.broadcast_action_to [ user, :hubs ],
      action: :update_attribute,
      targets: ".#{dom_id(self, :sidebar)}",
      content: active?.to_s,
      attributes: {
        attribute: "data-active"
      }
  end

  # Only broadcast when active? status actually transitions
  def health_status_changed?
    return true if saved_change_to_alive?
    return false unless saved_change_to_last_seen_at?

    # Check if last_seen_at change caused an active? transition
    old_last_seen, new_last_seen = saved_change_to_last_seen_at
    threshold = 2.minutes.ago

    was_active = alive? && old_last_seen.present? && old_last_seen > threshold
    is_active = alive? && new_last_seen.present? && new_last_seen > threshold

    was_active != is_active
  end

  # Broadcast hub health status to ActionCable health stream
  # JavaScript connections listen for these via hub:{id}:health stream
  def broadcast_health_status
    status = active? ? "online" : "offline"
    ActionCable.server.broadcast("hub:#{id}:health", { type: "health", cli: status })
    Rails.logger.debug "[Hub] Broadcast health transition: #{status}"
  end
end
