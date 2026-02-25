# frozen_string_literal: true

class GithubCommentIssueTool < ApplicationMCPTool
  tool_name "github_comment_issue"
  description "Add a comment to a GitHub issue or pull request. Requires the repository in 'owner/repo' format, issue number, and comment body."

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :issue_number, type: "integer", description: "Issue or PR number", required: true
  property :body, type: "string", description: "Comment text", required: true

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :issue_number, numericality: { only_integer: true, greater_than: 0 }
  validates :body, presence: true, length: { minimum: 1 }

  def perform
    # Check for cached idempotency response first
    cached_response = check_idempotency_cache
    if cached_response
      render(text: cached_response["text"])
      return
    end

    installation_id = ::Github::App.installation_id_for_repo(repo)
    unless installation_id
      report_error("GitHub App is not installed on #{repo}")
      return
    end

    client = ::Github::App.installation_client(installation_id)

    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Adding comment to #{repo}##{issue_number} as [bot] (#{client_info})...")

    # Add attribution footer to comment body
    enhanced_body = body + attribution_footer

    # Create comment using installation token (bot attribution)
    comment = client.add_comment(repo, issue_number.to_i, enhanced_body)

    success_message = [
      "✅ Comment added successfully!",
      "",
      "💬 Comment on #{repo}##{issue_number}",
      "   URL: #{comment[:html_url]}",
      "   Created: #{comment[:created_at].strftime('%Y-%m-%d %H:%M')}",
      "",
      "Preview:",
      "---",
      body.lines.first(5).join,
      (body.lines.count > 5 ? "... (truncated)" : ""),
      "---",
      "",
      "View full comment at: #{comment[:html_url]}"
    ]

    response_text = success_message.join("\n")
    store_idempotency_response(response_text)
    render(text: response_text)
  rescue Octokit::Error => e
    report_error("Failed to add comment: #{e.message}")
  rescue => e
    report_error("Error adding comment: #{e.message}")
  end
end
