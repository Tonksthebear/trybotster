# frozen_string_literal: true

module Integrations
  module Github
    module Webhooks
      class PrReviewCommentHandler < BaseHandler
        def call
          return unless processable?

          Rails.logger.info "Processing @trybotster mention in PR #{repo_full_name}##{pr_number}"

          target_issue_number, target_is_pr, routed_info = resolve_target

          create_github_message(
            repo: repo_full_name,
            issue_number: target_issue_number,
            comment_id: comment_id,
            comment_body: comment_body,
            comment_author: comment_author,
            issue_title: pr_title,
            issue_body: pr_body,
            issue_url: pr_url,
            is_pr: target_is_pr,
            source_type: "pr_review_comment",
            routed_info: routed_info,
            installation_id: installation_id
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
          linked_issue = fetch_linked_issue_for_pr(repo_full_name, pr_number)

          if linked_issue
            Rails.logger.info "PR ##{pr_number} links to issue ##{linked_issue}, routing to issue agent"

            routed_info = {
              source_number: pr_number,
              source_type: "pr",
              target_number: linked_issue,
              target_type: "issue",
              reason: "pr_linked_to_issue"
            }

            [ linked_issue, false, routed_info ]
          else
            [ pr_number, true, nil ]
          end
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

        def pr_number
          payload.dig("pull_request", "number")
        end

        def pr_title
          payload.dig("pull_request", "title")
        end

        def pr_body
          payload.dig("pull_request", "body")
        end

        def pr_url
          payload.dig("pull_request", "html_url")
        end
      end
    end
  end
end
