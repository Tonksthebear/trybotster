# frozen_string_literal: true

module Integrations
  module Github
    module Webhooks
      class IssueHandler < BaseHandler
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
          return if repo_full_name.blank? || issue_number.blank?

          Rails.logger.info "Processing closed Issue #{repo_full_name}##{issue_number}"

          create_cleanup_message(
            repo: repo_full_name,
            number: issue_number,
            is_pr: false,
            reason: "issue_closed"
          )

          Rails.logger.info "Created cleanup message for #{repo_full_name}##{issue_number}"
        end

        def handle_mention
          return if issue_body.blank?
          return if bot_author?(issue_author)
          return unless mentioned_trybotster?(issue_body)

          Rails.logger.info "Processing @trybotster mention in issue body #{repo_full_name}##{issue_number}"

          create_github_message(
            repo: repo_full_name,
            issue_number: issue_number,
            comment_id: nil,
            comment_body: issue_body,
            comment_author: issue_author,
            issue_title: issue_title,
            issue_body: issue_body,
            issue_url: issue_url,
            is_pr: false,
            installation_id: installation_id
          )
        end

        def issue_number
          payload.dig("issue", "number")
        end

        def issue_body
          payload.dig("issue", "body")
        end

        def issue_author
          payload.dig("issue", "user", "login")
        end

        def issue_title
          payload.dig("issue", "title")
        end

        def issue_url
          payload.dig("issue", "html_url")
        end
      end
    end
  end
end
