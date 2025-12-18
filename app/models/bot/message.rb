class Bot::Message < ApplicationRecord
  # Validations
  validates :event_type, presence: true, inclusion: {
    in: %w[github_mention manual_trigger system_notification agent_cleanup],
    message: "%{value} is not a valid event type"
  }
  validates :payload, presence: true
  validates :status, presence: true, inclusion: {
    in: %w[pending sent acknowledged failed],
    message: "%{value} is not a valid status"
  }

  # Scopes
  scope :pending, -> { where(status: "pending") }
  scope :sent, -> { where(status: "sent") }
  scope :acknowledged, -> { where(status: "acknowledged") }
  scope :failed, -> { where(status: "failed") }
  scope :unclaimed, -> { where(claimed_at: nil) }
  scope :claimed, -> { where.not(claimed_at: nil) }
  scope :for_delivery, -> { pending.unclaimed.order(created_at: :asc) }
  scope :stale, -> { sent.where("sent_at < ?", 5.minutes.ago).where(acknowledged_at: nil) }

  # Callbacks
  before_create :set_default_status

  # Custom error for already claimed messages
  class AlreadyClaimedError < StandardError; end

  # Instance methods
  def claim!(user_id)
    transaction do
      lock!  # Pessimistic lock to prevent race conditions

      if claimed?
        raise AlreadyClaimedError, "Message #{id} already claimed by user #{claimed_by_user_id}"
      end

      update!(
        claimed_by_user_id: user_id,
        claimed_at: Time.current,
        status: "sent",
        sent_at: Time.current
      )
    end
  end

  def mark_as_sent!
    update!(status: "sent", sent_at: Time.current)
  end

  def acknowledge!
    update!(status: "acknowledged", acknowledged_at: Time.current)

    # Add eyes reaction to the GitHub comment to provide user feedback
    add_eyes_reaction_to_comment
  end

  # Add eyes emoji reaction to the GitHub comment that triggered this message
  # This provides visual feedback that the bot saw the comment
  def add_eyes_reaction_to_comment
    return unless github_mention?
    return unless installation_id.present? && repo.present? && comment_id.present?

    result = Github::App.create_comment_reaction(
      installation_id,
      repo: repo,
      comment_id: comment_id,
      reaction: "eyes"
    )

    if result[:success]
      Rails.logger.info "Added eyes reaction to comment #{comment_id} in #{repo}"
    else
      Rails.logger.warn "Failed to add eyes reaction to comment #{comment_id}: #{result[:error]}"
    end
  rescue => e
    # Don't fail the acknowledge if reaction fails - it's just user feedback
    Rails.logger.error "Error adding eyes reaction: #{e.message}"
  end

  def mark_as_failed!(error_message = nil)
    payload_with_error = payload.merge(error: error_message) if error_message
    update!(status: "failed", payload: payload_with_error || payload)
  end

  def claimed?
    claimed_at.present?
  end

  def pending?
    status == "pending"
  end

  def sent?
    status == "sent"
  end

  def acknowledged?
    status == "acknowledged"
  end

  def failed?
    status == "failed"
  end

  def github_mention?
    event_type == "github_mention"
  end

  def agent_cleanup?
    event_type == "agent_cleanup"
  end

  # Extract common payload fields
  def repo
    payload["repo"]
  end

  def issue_number
    payload["issue_number"]
  end

  def comment_id
    payload["comment_id"]
  end

  def installation_id
    payload["installation_id"]
  end

  private

  def set_default_status
    self.status ||= "pending"
  end
end
