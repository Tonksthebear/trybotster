# frozen_string_literal: true

module Github
  module Webhooks
    class BaseHandler
      def initialize(payload)
        @payload = payload
      end

      def call
        raise NotImplementedError, "Subclasses must implement #call"
      end

      private

      attr_reader :payload

      def action
        payload["action"]
      end

      def repo_full_name
        payload.dig("repository", "full_name")
      end

      def mentioned_trybotster?(text)
        text.to_s.match?(/@trybotster\b/i)
      end

      def bot_author?(author)
        author == "trybotster" || author&.downcase&.include?("bot")
      end

      def create_bot_message(params)
        BotMessageCreator.new(params).call
      end

      def fetch_linked_issue_for_pr(repo, pr_number)
        LinkedIssueResolver.new(repo, pr_number).call
      end

      def create_cleanup_message(repo:, number:, is_pr:, reason:)
        Bot::Message.create!(
          event_type: "agent_cleanup",
          payload: {
            repo: repo,
            issue_number: number,
            is_pr: is_pr,
            reason: reason
          }
        )
      end
    end
  end
end
