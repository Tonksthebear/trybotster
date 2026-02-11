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

          token = valid_github_token
          return nil unless token

          installation_id = find_installation(token)
          return nil unless installation_id

          pr_body = fetch_pr_body(installation_id)
          return nil unless pr_body

          extract_first_linked_issue(pr_body)
        rescue => e
          Rails.logger.error "LinkedIssueResolver failed: #{e.class} - #{e.message}"
          Rails.logger.error e.backtrace.first(5).join("\n")
          nil
        end

        private

        def valid_github_token
          user = User.where.not(github_app_token: nil).first
          token = user&.valid_github_app_token

          unless token
            Rails.logger.warn "No valid GitHub App token available"
          end

          token
        end

        def find_installation(token)
          result = ::Github::App.get_installation_for_repo(token, @repo_full_name)

          unless result[:success]
            Rails.logger.warn "Could not find installation for repo: #{@repo_full_name}"
            return nil
          end

          result[:installation_id]
        end

        def fetch_pr_body(installation_id)
          token_result = ::Github::App.get_installation_token(installation_id)

          unless token_result[:success]
            Rails.logger.warn "Could not create installation token: #{token_result[:error]}"
            return nil
          end

          client = ::Github::App.client(token_result[:token])
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
