# frozen_string_literal: true

class GithubGetIssueCommentsTool < ApplicationMCPTool
  tool_name "github_get_issue_comments"
  description "Get all comments on a specific GitHub issue or pull request. Requires the repository in 'owner/repo' format and the issue number."

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :issue_number, type: "integer", description: "Issue or PR number", required: true

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :issue_number, numericality: { only_integer: true, greater_than: 0 }

  def perform
    # Check if user has authorized GitHub App
    unless current_user&.github_app_authorized?
      report_error("GitHub App not authorized. Please authorize at /github_app/authorize")
      return
    end

    # Get valid token (auto-refreshes if needed)
    token = current_user.valid_github_app_token
    unless token
      report_error("Failed to get valid GitHub token. Please re-authorize the GitHub App.")
      return
    end

    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Fetching comments for #{repo}##{issue_number} (via #{client_info})...")

    # Get comments using Octokit
    result = Github::App.get_issue_comments(token, repo: repo, issue_number: issue_number.to_i)

    if result && result[:success] && result[:comments]
      comments = result[:comments]

      if comments.empty?
        render(text: "No comments found on #{repo}##{issue_number}")
        return
      end

      output = [
        "💬 Comments on #{repo}##{issue_number} (#{comments.count} total)",
        "=" * 80,
        ""
      ]

      comments.each_with_index do |comment, index|
        author = comment[:user][:login]
        created_at = comment[:created_at]&.strftime("%Y-%m-%d %H:%M") || "Unknown"
        updated_at = comment[:updated_at]&.strftime("%Y-%m-%d %H:%M") || "Unknown"
        is_edited = comment[:created_at] != comment[:updated_at]

        output << "#{index + 1}. 👤 #{author} • 📅 #{created_at}#{is_edited ? ' (edited)' : ''}"
        output << "   🔗 #{comment[:html_url]}"
        output << ""

        # Show comment body
        if comment[:body].present?
          # Show first 500 characters of each comment
          body_preview = comment[:body][0..500]
          body_preview += "...\n(truncated)" if comment[:body].length > 500

          # Indent comment body
          body_preview.lines.each do |line|
            output << "   #{line.rstrip}"
          end
        else
          output << "   (No comment text)"
        end

        output << ""
        output << "-" * 80
        output << ""
      end

      output << "Total: #{comments.count} comment(s)"

      render(text: output.join("\n"))
    else
      error_msg = result&.dig(:error) || "Unknown error occurred"
      report_error("Failed to fetch comments: #{error_msg}")
    end
  rescue => e
    report_error("Error fetching comments: #{e.message}")
  end
end
