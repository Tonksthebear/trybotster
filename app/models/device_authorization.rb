class DeviceAuthorization < ApplicationRecord
  # User code alphabet - no ambiguous characters (0/O, 1/I/L, 5/S, A/4, U/V)
  USER_CODE_ALPHABET = "BCDFGHJKMNPQRTWXZ2346789".chars.freeze
  USER_CODE_LENGTH = 8
  DEVICE_CODE_LENGTH = 32
  DEFAULT_EXPIRES_IN = 15.minutes

  belongs_to :user, optional: true

  validates :device_code, presence: true, uniqueness: true
  validates :user_code, presence: true, uniqueness: true
  validates :expires_at, presence: true
  validates :status, presence: true, inclusion: { in: %w[pending approved denied expired] }

  before_validation :generate_codes, on: :create
  before_validation :set_expiration, on: :create

  scope :pending, -> { where(status: "pending") }
  scope :not_expired, -> { where("expires_at > ?", Time.current) }
  scope :expired, -> { where("expires_at <= ?", Time.current) }
  scope :valid_pending, -> { pending.not_expired }

  def expired?
    expires_at <= Time.current
  end

  def pending?
    status == "pending"
  end

  def approved?
    status == "approved"
  end

  def approve!(user)
    update!(user: user, status: "approved")
  end

  def deny!
    update!(status: "denied")
  end

  def expire!
    update!(status: "expired")
  end

  def expires_in
    [ (expires_at - Time.current).to_i, 0 ].max
  end

  def formatted_user_code
    "#{user_code[0..3]}-#{user_code[4..7]}"
  end

  # Cleanup expired authorizations (can be run periodically)
  def self.cleanup_expired!
    expired.where(status: "pending").update_all(status: "expired")
  end

  private

  def generate_codes
    self.device_code ||= SecureRandom.urlsafe_base64(DEVICE_CODE_LENGTH)
    self.user_code ||= generate_user_code
  end

  def generate_user_code
    loop do
      code = Array.new(USER_CODE_LENGTH) { USER_CODE_ALPHABET.sample }.join
      break code unless DeviceAuthorization.exists?(user_code: code)
    end
  end

  def set_expiration
    self.expires_at ||= DEFAULT_EXPIRES_IN.from_now
  end
end
