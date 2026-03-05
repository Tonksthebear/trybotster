# frozen_string_literal: true

class GithubPullRequestTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/pulls/{number}"
  template_name "github_pull_request"
  description "Get detailed information about a specific GitHub pull request, including diff and merge status."
  mime_type "application/json"

  parameter :owner, description: "Repository owner", required: true
  parameter :repo, description: "Repository name", required: true
  parameter :number, description: "Pull request number", required: true

  def resolve
    full_repo = "#{owner}/#{repo}"
    installation_id = ::Github::App.installation_id_for_repo(full_repo)
    raise "GitHub App is not installed on #{full_repo}" unless installation_id

    client = ::Github::App.installation_client(installation_id)
    pr = client.pull_request(full_repo, number.to_i)

    ActionMCP::Content::Resource.new(
      "github://repos/#{owner}/#{repo}/pulls/#{number}",
      "application/json",
      text: pr.to_h.to_json
    )
  rescue Octokit::Error => e
    raise "Failed to fetch pull request: #{e.message}"
  end
end
