# frozen_string_literal: true

module Integrations
  module Github
    module Webhooks
      class PrReviewHandler < BaseHandler
        def call
          return unless action == "submitted"
          return if repo_full_name.blank? || pr_number.blank?

          Rails.logger.info "Processing submitted PR review #{repo_full_name}##{pr_number}"

          create_pull_request_review_lifecycle_message(
            repo: repo_full_name,
            pr_number: pr_number,
            payload: {
              action: action,
              repo: repo_full_name,
              repository_full_name: repo_full_name,
              pr_number: pr_number,
              pull_request_number: pr_number,
              pr_url: pr_url,
              html_url: pr_url,
              head_branch: payload.dig("pull_request", "head", "ref"),
              base_branch: payload.dig("pull_request", "base", "ref"),
              review_id: review_id,
              review_url: review_url,
              review_html_url: review_html_url,
              reviewer: reviewer,
              state: review_state,
              body: review_body,
              submitted_at: submitted_at
            }
          )
        end

        private

        def pr_number
          payload.dig("pull_request", "number")
        end

        def pr_url
          payload.dig("pull_request", "html_url")
        end

        def review_id
          payload.dig("review", "id")
        end

        def review_url
          payload.dig("review", "url")
        end

        def review_html_url
          payload.dig("review", "html_url")
        end

        def reviewer
          payload.dig("review", "user", "login")
        end

        def review_state
          payload.dig("review", "state")
        end

        def review_body
          payload.dig("review", "body")
        end

        def submitted_at
          payload.dig("review", "submitted_at")
        end
      end
    end
  end
end
