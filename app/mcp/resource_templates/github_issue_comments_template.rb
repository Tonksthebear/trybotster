# frozen_string_literal: true

class GithubIssueCommentsTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/issues/{number}/comments"
  template_name "github_issue_comments"
  description "Get the general conversation comments on a GitHub issue or pull request."
  mime_type "application/json"

  parameter :owner, description: "Repository owner", required: true
  parameter :repo, description: "Repository name", required: true
  parameter :number, description: "Issue or PR number", required: true

  def resolve
    full_repo = "#{owner}/#{repo}"
    installation_id = ::Github::App.installation_id_for_repo(full_repo)
    raise "GitHub App is not installed on #{full_repo}" unless installation_id

    client = ::Github::App.installation_client(installation_id)
    comments = client.issue_comments(full_repo, number.to_i)

    ActionMCP::Content::Resource.new(
      "github://repos/#{owner}/#{repo}/issues/#{number}/comments",
      "application/json",
      text: comments.map(&:to_h).to_json
    )
  rescue Octokit::Error => e
    raise "Failed to fetch issue comments: #{e.message}"
  end
end
