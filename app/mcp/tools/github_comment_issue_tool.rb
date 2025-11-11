# frozen_string_literal: true

class GithubCommentIssueTool < ApplicationMCPTool
  tool_name "github_comment_issue"
  description "Add a comment to a GitHub issue or pull request. Requires the repository in 'owner/repo' format, issue number, and comment body."

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :issue_number, type: "number", description: "Issue or PR number", required: true
  property :body, type: "string", description: "Comment text", required: true

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :issue_number, numericality: { only_integer: true, greater_than: 0 }
  validates :body, presence: true, length: { minimum: 1 }

  def perform
    # Check if user has authorized GitHub App
    unless current_user&.github_app_authorized?
      report_error("GitHub App not authorized. Please authorize at /github_app/authorize")
      return
    end

    # Get installation ID
    unless current_user.github_app_installation_id.present?
      report_error("Installation ID not found. Please re-authorize the GitHub App.")
      return
    end

    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Adding comment to #{repo}##{issue_number} as [bot] (via #{client_info})...")

    # Get installation client (shows as [bot])
    begin
      client = GithubAppService.installation_client(current_user.github_app_installation_id)
    rescue => e
      report_error("Failed to get bot credentials: #{e.message}")
      return
    end

    # Add attribution footer to comment body
    enhanced_body = body + attribution_footer

    # Create comment using installation token (bot attribution)
    begin
      comment = client.add_comment(repo, issue_number.to_i, enhanced_body)
      result = { success: true, comment: comment.to_h }
    rescue Octokit::Error => e
      result = { success: false, error: e.message }
    end

    if result[:success]
      comment = result[:comment]

      success_message = [
        "✅ Comment added successfully!",
        "",
        "💬 Comment on #{repo}##{issue_number}",
        "   URL: #{comment[:html_url]}",
        "   Created: #{Time.parse(comment[:created_at]).strftime('%Y-%m-%d %H:%M')}",
        "",
        "Preview:",
        "---",
        body.lines.first(5).join,
        (body.lines.count > 5 ? "... (truncated)" : ""),
        "---",
        "",
        "View full comment at: #{comment[:html_url]}"
      ]

      render(text: success_message.join("\n"))
    else
      report_error("Failed to add comment: #{result[:error]}")
    end
  rescue => e
    report_error("Error adding comment: #{e.message}")
  end
end
