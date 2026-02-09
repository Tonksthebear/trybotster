# frozen_string_literal: true

class HubCommand < ApplicationRecord
  belongs_to :hub

  validates :event_type, presence: true, inclusion: {
    in: %w[browser_wants_preview],
    message: "%{value} is not a valid event type"
  }
  validates :payload, presence: true
  validates :status, presence: true, inclusion: {
    in: %w[pending acknowledged],
    message: "%{value} is not a valid status"
  }

  scope :unacked_from, ->(seq) { where("sequence > ?", seq).where.not(status: "acknowledged").order(sequence: :asc) }

  before_create :set_default_status
  after_create_commit :broadcast_to_hub_command_channel

  def self.create_for_hub!(hub, event_type:, payload:)
    seq = hub.next_message_sequence!
    create!(hub: hub, event_type: event_type, payload: payload, sequence: seq)
  end

  def acknowledge!
    update!(status: "acknowledged", acknowledged_at: Time.current)
  end

  def acknowledged?
    status == "acknowledged"
  end

  private

  def set_default_status
    self.status ||= "pending"
  end

  def broadcast_to_hub_command_channel
    ActionCable.server.broadcast(
      "hub_command:#{hub_id}",
      {
        type: "message",
        sequence: sequence,
        id: id,
        event_type: event_type,
        payload: payload,
        created_at: created_at.iso8601
      }
    )
  end
end
