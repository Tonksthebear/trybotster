class DeviceToken < ApplicationRecord
  TOKEN_PREFIX = "btstr_".freeze
  TOKEN_LENGTH = 32

  belongs_to :user

  encrypts :token, deterministic: true

  validates :token, presence: true, uniqueness: true

  before_validation :generate_token, on: :create

  scope :recently_used, -> { order(last_used_at: :desc) }

  def touch_usage!(ip: nil)
    update_columns(last_used_at: Time.current, last_ip: ip)
  end

  def display_token
    "#{TOKEN_PREFIX}...#{token.last(8)}"
  end

  private

  def generate_token
    self.token ||= "#{TOKEN_PREFIX}#{SecureRandom.urlsafe_base64(TOKEN_LENGTH)}"
  end
end
