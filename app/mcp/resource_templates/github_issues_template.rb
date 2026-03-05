# frozen_string_literal: true

class GithubIssuesTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/issues"
  template_name "github_issues"
  description "List GitHub issues for a repository. Returns all open issues by default."
  mime_type "application/json"

  parameter :owner, description: "Repository owner", required: true
  parameter :repo, description: "Repository name", required: true

  def resolve
    full_repo = "#{owner}/#{repo}"
    installation_id = ::Github::App.installation_id_for_repo(full_repo)
    raise "GitHub App is not installed on #{full_repo}" unless installation_id

    client = ::Github::App.installation_client(installation_id)
    issues = client.list_issues(full_repo, state: "open")

    ActionMCP::Content::Resource.new(
      "github://repos/#{owner}/#{repo}/issues",
      "application/json",
      text: issues.map(&:to_h).to_json
    )
  rescue Octokit::Error => e
    raise "Failed to fetch issues: #{e.message}"
  end
end
