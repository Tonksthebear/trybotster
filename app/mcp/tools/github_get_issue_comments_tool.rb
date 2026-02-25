# frozen_string_literal: true

class GithubGetIssueCommentsTool < ApplicationMCPTool
  tool_name "github_get_issue_comments"
  description "Get all comments on a specific GitHub issue or pull request. Requires the repository in 'owner/repo' format and the issue number."

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :issue_number, type: "integer", description: "Issue or PR number", required: true

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :issue_number, numericality: { only_integer: true, greater_than: 0 }

  def perform
    installation_id = ::Github::App.installation_id_for_repo(repo)
    unless installation_id
      report_error("GitHub App is not installed on #{repo}")
      return
    end

    client = ::Github::App.installation_client(installation_id)

    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Fetching comments for #{repo}##{issue_number} (via #{client_info})...")

    comments = client.issue_comments(repo, issue_number.to_i)

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
  rescue Octokit::Error => e
    report_error("Failed to fetch comments: #{e.message}")
  rescue => e
    report_error("Error fetching comments: #{e.message}")
  end
end
