# frozen_string_literal: true

class GithubIssueCommentsTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/issues/{number}/comments"
  template_name "github_issue_comments"
  description <<~DESC.strip
    Get the general conversation comments on a GitHub issue or pull request.

    This returns issue-style comments (the timeline thread) — NOT pull request
    reviews or inline diff comments. For review summaries and inline code comments
    on a PR, use the github_pull_request_reviews resource instead.
  DESC
  mime_type "application/json"

  parameter :owner, description: "Repository owner", required: true
  parameter :repo, description: "Repository name", required: true
  parameter :number, description: "Issue or PR number", required: true

  def resolve
    full_repo = "#{owner}/#{repo}"
    issue_number = number.to_i

    installation_id = ::Github::App.installation_id_for_repo(full_repo)
    unless installation_id
      return { error: "GitHub App is not installed on #{full_repo}" }.to_json
    end

    client = ::Github::App.installation_client(installation_id)
    comments = client.issue_comments(full_repo, issue_number)

    comments.map do |comment|
      {
        id: comment[:id],
        author: comment[:user][:login],
        created_at: comment[:created_at],
        updated_at: comment[:updated_at],
        html_url: comment[:html_url],
        body: comment[:body]
      }
    end.to_json
  rescue Octokit::Error => e
    { error: "Failed to fetch comments: #{e.message}" }.to_json
  rescue => e
    { error: "Error fetching comments: #{e.message}" }.to_json
  end

  private

  def owner
    arguments["owner"]
  end

  def repo
    arguments["repo"]
  end

  def number
    arguments["number"]
  end
end
