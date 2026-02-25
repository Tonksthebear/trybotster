# frozen_string_literal: true

module Integrations
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

        def installation_id
          payload.dig("installation", "id")
        end

        def mentioned_trybotster?(text)
          text.to_s.match?(/@trybotster\b/i)
        end

        def bot_author?(author)
          author == "trybotster" || author&.downcase&.end_with?("[bot]")
        end

        def collaborator?(username)
          return false if username.blank? || repo_full_name.blank? || installation_id.blank?

          result = ::Github::App.repo_collaborator?(installation_id, repo_full_name, username)
          unless result
            Rails.logger.info "Ignoring @trybotster mention from non-collaborator #{username} on #{repo_full_name}"
          end
          result
        end

        def create_github_message(params)
          MessageCreator.new(params).call
        end

        def fetch_linked_issue_for_pr(repo, pr_number)
          LinkedIssueResolver.new(repo, pr_number).call
        end

        def create_cleanup_message(repo:, number:, is_pr:, reason:)
          Integrations::Github::Message.create!(
            event_type: "agent_cleanup",
            repo: repo,
            issue_number: number,
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
end
