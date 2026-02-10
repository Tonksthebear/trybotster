# frozen_string_literal: true

# Device represents a registered client (CLI or browser) with E2E encryption keys.
#
# Two security modes:
# 1. Secure mode (CLI default): public_key is NULL on server.
#    Key exchange happens via QR code URL fragment - server cannot MITM.
#
# 2. Convenience mode: public_key IS stored on server.
#    Enables server-assisted pairing but allows potential MITM.
#
# Browser devices always store public_key (they need it for bidirectional key exchange).
class Device < ApplicationRecord
  belongs_to :user
  has_many :hubs, dependent: :destroy
  has_one :device_token, dependent: :destroy
  has_one :mcp_token, class_name: "Integrations::Github::MCPToken", dependent: :destroy

  # public_key is optional for CLI devices in secure mode
  # Browser devices always need public_key
  validates :public_key, presence: true, if: :browser?
  validates :public_key, uniqueness: true, allow_nil: true
  validates :device_type, presence: true, inclusion: { in: %w[cli browser] }
  validates :name, presence: true
  validates :fingerprint, presence: true, uniqueness: { scope: :user_id }

  before_validation :generate_fingerprint, on: :create

  scope :cli_devices, -> { where(device_type: "cli") }
  scope :browser_devices, -> { where(device_type: "browser") }
  scope :active, -> { where("last_seen_at > ?", 5.minutes.ago) }
  scope :by_last_seen, -> { order(last_seen_at: :desc) }

  def cli?
    device_type == "cli"
  end

  def browser?
    device_type == "browser"
  end

  def active?
    last_seen_at.present? && last_seen_at > 5.minutes.ago
  end

  # Device is in secure mode if public_key is NOT stored on server (CLI only)
  # In secure mode, key exchange must happen via QR code URL fragment
  def secure_mode?
    cli? && public_key.blank?
  end

  # Device supports server-assisted pairing if public_key IS stored
  def server_assisted_pairing?
    public_key.present?
  end

  def touch_last_seen!
    update_column(:last_seen_at, Time.current)
  end

  private

  # Generate human-verifiable fingerprint from public key.
  # Users can compare fingerprints to verify device identity.
  # Format: "a3:f2:91:cc:b7:e4:22:af" (8 hex bytes from SHA256)
  def generate_fingerprint
    return if public_key.blank?

    hash = Digest::SHA256.digest(public_key)[0, 8]
    self.fingerprint = hash.bytes.map { |b| b.to_s(16).rjust(2, "0") }.join(":")
  end
end
