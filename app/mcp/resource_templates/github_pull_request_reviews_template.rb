# frozen_string_literal: true

class GithubPullRequestReviewsTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/pulls/{number}/reviews"
  template_name "github_pull_request_reviews"
  description "Get all reviews and inline code comments on a GitHub pull request."
  mime_type "application/json"

  parameter :owner, description: "Repository owner", required: true
  parameter :repo, description: "Repository name", required: true
  parameter :number, description: "Pull request number", required: true

  def resolve
    full_repo = "#{owner}/#{repo}"
    installation_id = ::Github::App.installation_id_for_repo(full_repo)
    raise "GitHub App is not installed on #{full_repo}" unless installation_id

    client = ::Github::App.installation_client(installation_id)
    reviews = client.pull_request_reviews(full_repo, number.to_i)
    comments = client.pull_request_comments(full_repo, number.to_i)

    ActionMCP::Content::Resource.new(
      "github://repos/#{owner}/#{repo}/pulls/#{number}/reviews",
      "application/json",
      text: { reviews: reviews.map(&:to_h), inline_comments: comments.map(&:to_h) }.to_json
    )
  rescue Octokit::Error => e
    raise "Failed to fetch pull request reviews: #{e.message}"
  end
end
