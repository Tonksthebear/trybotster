# frozen_string_literal: true

class GithubIssueTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/issues/{number}"
  template_name "github_issue"
  description "Get detailed information about a specific GitHub issue or pull request."
  mime_type "application/json"

  parameter :owner, description: "Repository owner", required: true
  parameter :repo, description: "Repository name", required: true
  parameter :number, description: "Issue or PR number", required: true

  def resolve
    full_repo = "#{owner}/#{repo}"
    installation_id = ::Github::App.installation_id_for_repo(full_repo)
    raise "GitHub App is not installed on #{full_repo}" unless installation_id

    client = ::Github::App.installation_client(installation_id)
    issue = client.issue(full_repo, number.to_i)

    ActionMCP::Content::Resource.new(
      "github://repos/#{owner}/#{repo}/issues/#{number}",
      "application/json",
      text: issue.to_h.to_json
    )
  rescue Octokit::Error => e
    raise "Failed to fetch issue: #{e.message}"
  end
end
