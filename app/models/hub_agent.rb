# frozen_string_literal: true

class HubAgent < ApplicationRecord
  belongs_to :hub

  validates :session_key, presence: true
  validates :session_key, uniqueness: { scope: :hub_id }

  # Delegate user access through hub
  delegate :user, to: :hub
end
