# frozen_string_literal: true

# WebRTC signaling session for P2P browser-to-CLI connections
# The Rails server only handles signaling (offer/answer exchange) -
# actual data flows directly between browser and CLI via WebRTC data channels.
class WebrtcSession < ApplicationRecord
  # Associations
  belongs_to :user

  # Validations
  validates :offer, presence: true
  validates :status, presence: true, inclusion: {
    in: %w[pending answered connected expired failed],
    message: "%{value} is not a valid status"
  }
  validates :expires_at, presence: true

  # Scopes
  scope :pending, -> { where(status: "pending") }
  scope :answered, -> { where(status: "answered") }
  scope :active, -> { where(status: %w[pending answered connected]) }
  scope :expired, -> { where("expires_at < ?", Time.current) }
  scope :for_user, ->(user) { where(user: user) }

  # Callbacks
  before_validation :set_defaults, on: :create

  # Instance methods

  # Set the answer SDP from the CLI
  def set_answer!(answer_data)
    update!(
      answer: answer_data,
      status: "answered"
    )
  end

  # Mark as connected once the P2P connection is established
  def mark_connected!
    update!(status: "connected")
  end

  # Mark as expired
  def mark_expired!
    update!(status: "expired")
  end

  # Mark as failed
  def mark_failed!(error_message = nil)
    update!(
      status: "failed",
      offer: offer.merge("error" => error_message)
    )
  end

  # Status predicates
  def pending?
    status == "pending"
  end

  def answered?
    status == "answered"
  end

  def connected?
    status == "connected"
  end

  def expired?
    status == "expired" || expires_at < Time.current
  end

  def failed?
    status == "failed"
  end

  private

  def set_defaults
    self.status ||= "pending"
    self.expires_at ||= 5.minutes.from_now
  end
end
