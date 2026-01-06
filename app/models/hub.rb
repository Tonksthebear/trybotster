# frozen_string_literal: true

class Hub < ApplicationRecord
  belongs_to :user
  belongs_to :device, optional: true  # The CLI device running this hub
  has_many :hub_agents, dependent: :destroy

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
end
