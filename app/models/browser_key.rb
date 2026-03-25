# frozen_string_literal: true

# BrowserKey represents a browser's E2E encryption key registration.
#
# Browsers always store public_key (needed for bidirectional key exchange).
class BrowserKey < ApplicationRecord
  belongs_to :user

  validates :public_key, presence: true, uniqueness: true
  validates :name, presence: true
  validates :fingerprint, presence: true, uniqueness: { scope: :user_id }

  before_validation :generate_fingerprint, on: :create

  scope :active, -> { where("last_seen_at > ?", 5.minutes.ago) }
  scope :by_last_seen, -> { order(last_seen_at: :desc) }

  def active?
    last_seen_at.present? && last_seen_at > 5.minutes.ago
  end

  def touch_last_seen!
    update_column(:last_seen_at, Time.current)
  end

  private

  # Generate human-verifiable fingerprint from public key.
  # Format: "a3:f2:91:cc:b7:e4:22:af" (8 hex bytes from SHA256)
  def generate_fingerprint
    return if public_key.blank?

    hash = Digest::SHA256.digest(public_key)[0, 8]
    self.fingerprint = hash.bytes.map { |b| b.to_s(16).rjust(2, "0") }.join(":")
  end
end
