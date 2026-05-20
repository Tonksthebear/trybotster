# frozen_string_literal: true

class GithubMarkPullRequestReadyForReviewTool < ApplicationMCPTool
  tool_name "github_mark_pull_request_ready_for_review"
  description "Mark an existing GitHub draft pull request ready for review. Requires repository in 'owner/repo' format and pull request number."

  property :repo, type: "string", description: "Repository in 'owner/repo' format. Defaults to the calling Botster session repo when available.", required: false
  property :pr_number, type: "integer", description: "Pull request number", required: true

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :pr_number, numericality: { only_integer: true, greater_than: 0 }

  MARK_READY_MUTATION = <<~GRAPHQL.squish
    mutation($id: ID!) {
      markPullRequestReadyForReview(input: { pullRequestId: $id }) {
        pullRequest {
          number
          isDraft
          url
        }
      }
    }
  GRAPHQL

  def perform
    installation_id = ::Github::App.installation_id_for_repo(repo)
    unless installation_id
      report_error("GitHub App is not installed on #{repo}")
      return
    end

    client = ::Github::App.installation_client(installation_id)
    agent_info = botster_session_attribution
    render(text: "Marking #{repo}##{pr_number} ready for review as [bot] (#{agent_info})...")

    pr = client.pull_request(repo, pr_number.to_i)
    if pr[:draft] == false
      render(text: "✅ Pull request #{repo}##{pr_number} is already ready for review.")
      return
    end

    node_id = pr[:node_id].presence
    unless node_id
      report_error("GitHub did not return a GraphQL node id for #{repo}##{pr_number}; cannot mark ready for review.")
      return
    end

    response = client.post("/graphql", { query: MARK_READY_MUTATION, variables: { id: node_id } }.to_json)
    response_hash = github_response_hash(response)
    errors = response_hash[:errors]

    if errors.present?
      messages = Array(errors).filter_map { |error| error[:message] || error["message"] }
      if messages.any? { |message| message.to_s.match?(/already.*ready|not.*draft/i) }
        render(text: "✅ Pull request #{repo}##{pr_number} is already ready for review.")
      else
        report_error("Failed to mark pull request ready for review: #{messages.presence&.join('; ') || errors.inspect}")
      end
      return
    end

    pull_request = response_hash.dig(:data, :markPullRequestReadyForReview, :pullRequest) ||
      response_hash.dig("data", "markPullRequestReadyForReview", "pullRequest") ||
      {}

    if pull_request[:isDraft] == false || pull_request["isDraft"] == false
      url = pull_request[:url] || pull_request["url"] || pr[:html_url]
      render(text: "✅ Pull request #{repo}##{pr_number} is ready for review.\n\n🔗 URL: #{url}")
    else
      report_error("GitHub did not confirm that #{repo}##{pr_number} is ready for review.")
    end
  rescue Octokit::Error => e
    report_error("Failed to mark pull request ready for review: #{e.message}")
  rescue => e
    report_error("Error marking pull request ready for review: #{e.message}")
  end

  private

  def github_response_hash(response)
    response = response.to_h if response.respond_to?(:to_h)
    response.is_a?(Hash) ? response : {}
  end
end
