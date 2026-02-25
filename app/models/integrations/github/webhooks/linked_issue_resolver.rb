# frozen_string_literal: true

module Integrations
  module Github
    module Webhooks
      # Resolves linked issue numbers from PR bodies
      # Supports: Fixes #123, Closes #123, Resolves #123, References #123
      class LinkedIssueResolver
        LINKING_KEYWORDS_PATTERN = /(?:fix(?:es|ed)?|close(?:s|d)?|resolve(?:s|d)?|references?)\s+#(\d+)/i

        def initialize(repo_full_name, pr_number)
          @repo_full_name = repo_full_name
          @pr_number = pr_number
        end

        def call
          Rails.logger.info "LinkedIssueResolver: repo=#{@repo_full_name}, pr=#{@pr_number}"

          installation_id = ::Github::App.installation_id_for_repo(@repo_full_name)
          unless installation_id
            Rails.logger.warn "GitHub App not installed on #{@repo_full_name}"
            return nil
          end

          pr_body = fetch_pr_body(installation_id)
          return nil unless pr_body

          extract_first_linked_issue(pr_body)
        rescue => e
          Rails.logger.error "LinkedIssueResolver failed: #{e.class} - #{e.message}"
          Rails.logger.error e.backtrace.first(5).join("\n")
          nil
        end

        private

        def fetch_pr_body(installation_id)
          client = ::Github::App.installation_client(installation_id)
          pr = client.pull_request(@repo_full_name, @pr_number)
          pr.body
        end

        def extract_first_linked_issue(pr_body)
          return nil if pr_body.blank?

          issue_numbers = []

          pr_body.scan(LINKING_KEYWORDS_PATTERN) do |match|
            issue_numbers << match[0].to_i
          end

          result = issue_numbers.uniq.first
          Rails.logger.info "LinkedIssueResolver: extracted issue=#{result.inspect}"
          result
        end
      end
    end
  end
end
