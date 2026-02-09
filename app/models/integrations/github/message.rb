# frozen_string_literal: true

module Integrations
  module Github
    class Message < ApplicationRecord
      self.table_name = "github_messages"

      validates :event_type, presence: true, inclusion: {
        in: %w[github_mention agent_cleanup],
        message: "%{value} is not a valid event type"
      }
      validates :repo, presence: true
      validates :payload, presence: true
      validates :status, presence: true, inclusion: {
        in: %w[pending acknowledged],
        message: "%{value} is not a valid status"
      }

      scope :pending, -> { where(status: "pending") }
      scope :acknowledged, -> { where(status: "acknowledged") }
      scope :for_repo, ->(repo) { where(repo: repo) }

      before_create :set_default_status
      after_create_commit :broadcast_to_repo_stream

      def acknowledge!
        update!(status: "acknowledged", acknowledged_at: Time.current)
        add_eyes_reaction
      end

      def acknowledged?
        status == "acknowledged"
      end

      def github_mention?
        event_type == "github_mention"
      end

      # Wire format for ActionCable transmission to CLI via Github::EventsChannel
      def to_wire
        {
          type: "message",
          id: id,
          event_type: event_type,
          payload: payload,
          repo: repo,
          created_at: created_at.iso8601
        }
      end

      # Payload accessors
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

      def broadcast_to_repo_stream
        ActionCable.server.broadcast("github_events:#{repo}", to_wire)
      end

      # Add eyes emoji reaction to the GitHub comment or issue that triggered this message
      def add_eyes_reaction
        return unless github_mention?
        return unless installation_id.present? && repo.present?

        if comment_id.present?
          result = ::Github::App.create_comment_reaction(
            installation_id,
            repo: repo,
            comment_id: comment_id,
            reaction: "eyes"
          )
          target = "comment #{comment_id}"
        elsif issue_number.present?
          result = ::Github::App.create_issue_reaction(
            installation_id,
            repo: repo,
            issue_number: issue_number,
            reaction: "eyes"
          )
          target = "issue ##{issue_number}"
        else
          Rails.logger.warn "Cannot add reaction: no comment_id or issue_number"
          return
        end

        if result[:success]
          Rails.logger.info "Added eyes reaction to #{target} in #{repo}"
        else
          Rails.logger.warn "Failed to add eyes reaction to #{target}: #{result[:error]}"
        end
      rescue => e
        Rails.logger.error "Error adding eyes reaction: #{e.message}"
      end
    end
  end
end
