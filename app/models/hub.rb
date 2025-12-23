# frozen_string_literal: true

class Hub < ApplicationRecord
  belongs_to :user
  has_many :hub_agents, dependent: :destroy

  validates :repo, presence: true
  validates :identifier, presence: true, uniqueness: true
  validates :last_seen_at, presence: true

  scope :active, -> { where("last_seen_at > ?", 2.minutes.ago) }
  scope :for_repo, ->(repo) { where(repo: repo) }
  scope :stale, -> { where("last_seen_at <= ?", 2.minutes.ago) }
end
