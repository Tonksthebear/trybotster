# frozen_string_literal: true

class IdempotencyKey < ApplicationRecord
  # Retention period for idempotency keys (24 hours)
  RETENTION_PERIOD = 24.hours

  validates :key, presence: true, uniqueness: true
  validates :request_path, presence: true

  # Check if this idempotency key has a completed response
  def completed?
    completed_at.present?
  end

  # Check if this key has expired (older than retention period)
  def expired?
    created_at < RETENTION_PERIOD.ago
  end

  # Mark this key as completed with response data
  def mark_completed!(status:, body:)
    update!(
      response_status: status,
      response_body: body,
      completed_at: Time.current
    )
  end

  class << self
    # Find an existing key or create a new one for the request
    def find_or_create_for_request(key, request_path, request_params = nil)
      find_or_create_by!(key: key) do |idempotency_key|
        idempotency_key.request_path = request_path
        idempotency_key.request_params = request_params.to_json if request_params
      end
    rescue ActiveRecord::RecordNotUnique
      # Handle race condition - another request created the key
      find_by!(key: key)
    end

    # Remove expired idempotency keys
    def cleanup_expired
      where("created_at < ?", RETENTION_PERIOD.ago).delete_all
    end
  end
end
