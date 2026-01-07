# frozen_string_literal: true

class HubAgent < ApplicationRecord
  belongs_to :hub

  validates :session_key, presence: true
  validates :session_key, uniqueness: { scope: :hub_id }
  validates :tunnel_port, numericality: { greater_than: 0, less_than: 65536 }, allow_nil: true
  validates :tunnel_status, inclusion: { in: %w[disconnected connected] }

  # Delegate user access through hub
  delegate :user, to: :hub

  # Tunnel scopes
  scope :with_tunnel, -> { where.not(tunnel_port: nil) }
  scope :tunnel_connected, -> { where(tunnel_status: "connected") }

  # Tunnel status methods
  def tunnel_connected?
    tunnel_status == "connected"
  end

  def mark_tunnel_connected!
    update!(tunnel_status: "connected", tunnel_connected_at: Time.current)
  end

  def mark_tunnel_disconnected!
    update!(tunnel_status: "disconnected")
  end
end
