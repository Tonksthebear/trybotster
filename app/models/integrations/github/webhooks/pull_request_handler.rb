# frozen_string_literal: true

module Integrations
  module Github
    module Webhooks
      class PullRequestHandler < BaseHandler
        def call
          case action
          when "closed"
            handle_closed
          when "opened", "edited"
            handle_mention
          end
        end

        private

        def handle_closed
          return if repo_full_name.blank? || pr_number.blank?

          Rails.logger.info "Processing closed PR #{repo_full_name}##{pr_number}"

          if merged?
            create_pull_request_lifecycle_message(
              repo: repo_full_name,
              pr_number: pr_number,
              payload: {
                action: "closed",
                repo: repo_full_name,
                repository_full_name: repo_full_name,
                pr_number: pr_number,
                pull_request_number: pr_number,
                pr_url: pr_url,
                html_url: pr_url,
                head_branch: payload.dig("pull_request", "head", "ref"),
                base_branch: payload.dig("pull_request", "base", "ref"),
                merge_commit: merge_commit_sha,
                merge_commit_sha: merge_commit_sha,
                merged: true,
                merged_at: merged_at
              }
            )
          end

          create_cleanup_message(
            repo: repo_full_name,
            number: pr_number,
            is_pr: true,
            reason: "pr_closed"
          )

          Rails.logger.info "Created cleanup message for #{repo_full_name}##{pr_number}"
        end

        def handle_mention
          return if pr_body.blank?
          return if bot_author?(pr_author)
          return unless mentioned_trybotster?(pr_body)
          return unless collaborator?(pr_author)

          Rails.logger.info "Processing @trybotster mention in PR body #{repo_full_name}##{pr_number}"

          create_github_message(
            repo: repo_full_name,
            issue_number: pr_number,
            comment_id: nil,
            comment_body: pr_body,
            comment_author: pr_author,
            issue_title: pr_title,
            issue_body: pr_body,
            issue_url: pr_url,
            is_pr: true,
            installation_id: installation_id
          )
        end

        def pr_number
          payload.dig("pull_request", "number")
        end

        def pr_body
          payload.dig("pull_request", "body")
        end

        def pr_author
          payload.dig("pull_request", "user", "login")
        end

        def pr_title
          payload.dig("pull_request", "title")
        end

        def pr_url
          payload.dig("pull_request", "html_url")
        end

        def merged?
          payload.dig("pull_request", "merged") == true
        end

        def merged_at
          payload.dig("pull_request", "merged_at")
        end

        def merge_commit_sha
          payload.dig("pull_request", "merge_commit_sha")
        end
      end
    end
  end
end
