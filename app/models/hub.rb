# frozen_string_literal: true

class Hub < ApplicationRecord
  belongs_to :user
  belongs_to :device, optional: true  # The CLI device running this hub
  has_many :hub_commands, dependent: :destroy

  validates :identifier, presence: true, uniqueness: true
  validates :last_seen_at, presence: true

  scope :active, -> { where(alive: true).where("last_seen_at > ?", 2.minutes.ago) }
  scope :stale, -> { where(alive: false).or(where("last_seen_at <= ?", 2.minutes.ago)) }
  scope :with_device, -> { where.not(device_id: nil) }

  after_commit :broadcast_hubs_list
  after_create_commit :broadcast_redirect_to_hub
  after_update_commit :broadcast_health_status, if: :health_status_changed?
  after_destroy_commit :broadcast_health_offline

  # Check if this hub supports E2E encrypted terminal access
  def e2e_enabled?
    device.present?
  end

  # Check if this hub is active (alive flag set and seen within 2 minutes)
  def active?
    alive? && last_seen_at > 2.minutes.ago
  end

  # Display name for the hub
  def name
    read_attribute(:name).presence || device&.name || identifier.truncate(20)
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

  def broadcast_redirect_to_hub
    Turbo::StreamsChannel.broadcast_action_to(
      [ user, :hubs ],
      action: :redirect,
      attributes: { url: Rails.application.routes.url_helpers.hub_path(self), from: "/hubs" }
    )
  rescue => e
    Rails.logger.warn "Failed to broadcast hub redirect: #{e.message}"
  end

  def broadcast_hubs_list
    hubs = user.hubs.includes(:device).order(last_seen_at: :desc)

    Turbo::StreamsChannel.broadcast_update_to(
      [ user, :hubs ],
      targets: ".hubs-list",
      partial: "layouts/sidebar_hubs",
      locals: { hubs: hubs }
    )

    Turbo::StreamsChannel.broadcast_update_to(
      [ user, :hubs ],
      targets: ".hubs-dashboard",
      partial: "hubs/index_hubs",
      locals: { hubs: hubs }
    )
  rescue => e
    Rails.logger.warn "Failed to broadcast hubs list: #{e.message}"
  end

  def broadcast_health_offline
    ActionCable.server.broadcast("hub:#{id}:health", { type: "health", cli: "offline" })
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
