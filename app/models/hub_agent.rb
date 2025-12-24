# frozen_string_literal: true

class HubAgent < ApplicationRecord
  belongs_to :hub

  validates :session_key, presence: true
  validates :session_key, uniqueness: { scope: :hub_id }
  validates :tunnel_port, numericality: { greater_than: 0, less_than: 65536 }, allow_nil: true
  validates :tunnel_status, inclusion: { in: %w[disconnected connected] }
  validates :tunnel_share_token, uniqueness: true, allow_nil: true

  # Delegate user access through hub
  delegate :user, to: :hub

  # Tunnel scopes
  scope :with_tunnel, -> { where.not(tunnel_port: nil) }
  scope :tunnel_connected, -> { where(tunnel_status: "connected") }
  scope :shared, -> { where(tunnel_share_enabled: true).where.not(tunnel_share_token: nil) }

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

  # Public sharing methods
  def enable_sharing!
    update!(
      tunnel_share_token: SecureRandom.urlsafe_base64(16),
      tunnel_share_enabled: true
    )
  end

  def disable_sharing!
    update!(tunnel_share_token: nil, tunnel_share_enabled: false)
  end

  def sharing_enabled?
    tunnel_share_enabled && tunnel_share_token.present?
  end

  def share_url
    return nil unless sharing_enabled?

    Rails.application.routes.url_helpers.shared_tunnel_url(token: tunnel_share_token)
  end
end
