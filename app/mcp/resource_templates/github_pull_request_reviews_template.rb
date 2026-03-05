# frozen_string_literal: true

class GithubPullRequestReviewsTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/pulls/{number}/reviews"
  template_name "github_pull_request_reviews"
  description <<~DESC.strip
    Get all reviews and inline code comments on a GitHub pull request.

    GitHub PRs have two distinct comment types — this resource fetches both:

    1. REVIEWS — top-level review submissions (APPROVE / REQUEST_CHANGES / COMMENT)
       with their summary body. Fetched via the Reviews API.

    2. INLINE COMMENTS — line-level comments on specific diff lines, attached to
       a review. Fetched via the Pull Request Review Comments API.

    Note: this resource does NOT return general issue/conversation comments (the timeline
    comments posted directly on the PR). Use the github_issue_comments resource for those.
  DESC
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

    comments_by_review = comments.group_by { |c| c[:pull_request_review_id] }

    ActionMCP::Content::Resource.new(
      "github://repos/#{owner}/#{repo}/pulls/#{number}/reviews",
      "application/json",
      text: {
        reviews: reviews.map { |review|
          inline = comments_by_review[review[:id]]
          {
            id: review[:id],
            state: review[:state],
            author: review[:user][:login],
            submitted_at: review[:submitted_at],
            html_url: review[:html_url],
            body: review[:body],
            inline_comments: inline&.map { |c|
              {
                path: c[:path],
                line: c[:line] || c[:original_line],
                author: c[:user][:login],
                created_at: c[:created_at],
                html_url: c[:html_url],
                body: c[:body]
              }
            }
          }.compact
        },
        standalone_inline_comments: (comments_by_review[nil] || []).map { |c|
          {
            path: c[:path],
            line: c[:line] || c[:original_line],
            author: c[:user][:login],
            created_at: c[:created_at],
            html_url: c[:html_url],
            body: c[:body]
          }
        },
        summary: {
          total_reviews: reviews.count,
          total_inline_comments: comments.count
        }
      }.to_json
    )
  rescue Octokit::Error => e
    raise "Failed to fetch pull request reviews: #{e.message}"
  end
end
