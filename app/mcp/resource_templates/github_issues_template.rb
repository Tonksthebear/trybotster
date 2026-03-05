# frozen_string_literal: true

class GithubIssuesTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/issues"
  template_name "github_issues"
  description "List GitHub issues for a repository. Can filter by state: open, closed, or all."
  mime_type "application/json"

  parameter :owner, description: "Repository owner", required: true
  parameter :repo, description: "Repository name", required: true
  parameter :state, description: "State: open, closed, all (default: open)", required: false

  def resolve
    full_repo = "#{owner}/#{repo}"
    installation_id = ::Github::App.installation_id_for_repo(full_repo)
    raise "GitHub App is not installed on #{full_repo}" unless installation_id

    client = ::Github::App.installation_client(installation_id)
    state_param = state.presence || "open"
    issues = client.list_issues(full_repo, state: state_param)

    ActionMCP::Content::Resource.new(
      "github://repos/#{owner}/#{repo}/issues",
      "application/json",
      text: issues.map { |issue|
        {
          number: issue[:number],
          title: issue[:title],
          state: issue[:state],
          html_url: issue[:html_url],
          author: issue[:user][:login],
          comments: issue[:comments],
          created_at: issue[:created_at],
          updated_at: issue[:updated_at],
          labels: issue[:labels]&.map { |l| l[:name] },
          is_pull_request: issue[:pull_request].present?
        }
      }.to_json
    )
  rescue Octokit::Error => e
    raise "Failed to fetch issues: #{e.message}"
  end
end
