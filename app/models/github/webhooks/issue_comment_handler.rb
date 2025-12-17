# frozen_string_literal: true

module Github
  module Webhooks
    class IssueCommentHandler < BaseHandler
      def call
        return unless processable?

        Rails.logger.info "Processing @trybotster mention in #{repo_full_name}##{issue_number}"

        target_issue_number, target_is_pr, routed_info = resolve_target

        create_bot_message(
          repo: repo_full_name,
          issue_number: target_issue_number,
          comment_id: comment_id,
          comment_body: comment_body,
          comment_author: comment_author,
          issue_title: issue_title,
          issue_body: issue_body,
          issue_url: issue_url,
          is_pr: target_is_pr,
          source_type: source_type,
          routed_info: routed_info
        )
      end

      private

      def processable?
        return false unless %w[created edited].include?(action)
        return false if comment_body.blank?
        return false if bot_author?(comment_author)
        return false unless mentioned_trybotster?(comment_body)

        true
      end

      def resolve_target
        routed_info = nil

        if pr_comment?
          linked_issue = fetch_linked_issue_for_pr(repo_full_name, issue_number)

          if linked_issue
            Rails.logger.info "PR ##{issue_number} links to issue ##{linked_issue}, routing to issue agent"

            routed_info = {
              source_number: issue_number,
              source_type: "pr",
              target_number: linked_issue,
              target_type: "issue",
              reason: "pr_linked_to_issue"
            }

            return [linked_issue, false, routed_info]
          end
        end

        [issue_number, pr_comment?, nil]
      end

      def comment_body
        payload.dig("comment", "body")
      end

      def comment_author
        payload.dig("comment", "user", "login")
      end

      def comment_id
        payload.dig("comment", "id")
      end

      def issue_number
        payload.dig("issue", "number")
      end

      def issue_title
        payload.dig("issue", "title")
      end

      def issue_body
        payload.dig("issue", "body")
      end

      def issue_url
        payload.dig("issue", "html_url")
      end

      def pr_comment?
        payload.dig("issue", "pull_request").present?
      end

      def source_type
        pr_comment? ? "pr_comment" : "issue_comment"
      end
    end
  end
end
