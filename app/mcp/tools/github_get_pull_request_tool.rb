# frozen_string_literal: true

class GithubGetPullRequestTool < ApplicationMCPTool
  tool_name "github_get_pull_request"
  description "Get detailed information about a specific GitHub pull request, including diff and merge status. Requires repository in 'owner/repo' format and PR number."

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :pr_number, type: "number", description: "Pull request number", required: true

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :pr_number, numericality: { only_integer: true, greater_than: 0 }

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
    render(text: "Fetching details for #{repo}##{pr_number} (via #{client_info})...")

    # Get PR using Octokit
    result = Github::App.get_issue(token, repo: repo, issue_number: pr_number.to_i)

    if result[:success]
      pr = result[:issue]

      unless pr["pull_request"].present?
        report_error("Issue ##{pr_number} is not a pull request")
        return
      end

      pr_details = [
        "🔀 Pull Request ##{pr['number']}: #{pr['title']}",
        "=" * 60,
        "",
        "📍 Repository: #{repo}",
        "🔗 URL: #{pr['html_url']}",
        "📊 State: #{pr['state']}",
        "👤 Author: #{pr['user']['login']}",
        "💬 Comments: #{pr['comments']}",
        "",
        "📅 Created: #{Time.parse(pr['created_at']).strftime('%Y-%m-%d %H:%M')}",
        "🔄 Updated: #{Time.parse(pr['updated_at']).strftime('%Y-%m-%d %H:%M')}"
      ]

      if pr["merged_at"]
        pr_details << "✅ Merged: #{Time.parse(pr['merged_at']).strftime('%Y-%m-%d %H:%M')}"
      elsif pr["closed_at"]
        pr_details << "❌ Closed: #{Time.parse(pr['closed_at']).strftime('%Y-%m-%d %H:%M')}"
      end

      if pr["assignees"]&.any?
        assignees = pr["assignees"].map { |a| a["login"] }.join(", ")
        pr_details << "👥 Assignees: #{assignees}"
      end

      if pr["labels"]&.any?
        labels = pr["labels"].map { |l| "#{l['name']}" }.join(", ")
        pr_details << "🏷️  Labels: #{labels}"
      end

      pr_details << ""
      pr_details << "🔀 Branch Info:"
      pr_details << "   Head: #{pr['head']&.dig('ref') || 'unknown'} (#{pr['head']&.dig('repo', 'full_name') || 'unknown'})"
      pr_details << "   Base: #{pr['base']&.dig('ref') || 'unknown'}"

      if pr["mergeable_state"]
        pr_details << "   Mergeable State: #{pr['mergeable_state']}"
      end

      pr_details << ""
      pr_details << "📝 Description:"
      pr_details << "-" * 60

      if pr["body"].present?
        body_preview = pr["body"][0..1000]
        body_preview += "...\n(truncated)" if pr["body"].length > 1000
        pr_details << body_preview
      else
        pr_details << "(No description provided)"
      end

      pr_details << "-" * 60

      if pr["pull_request"]
        pr_details << ""
        pr_details << "📌 Additional PR Details:"
        pr_details << "   Diff URL: #{pr['pull_request']['diff_url']}" if pr["pull_request"]["diff_url"]
        pr_details << "   Patch URL: #{pr['pull_request']['patch_url']}" if pr["pull_request"]["patch_url"]
      end

      render(text: pr_details.join("\n"))
    else
      report_error("Failed to fetch pull request: #{result[:error]}")
    end
  rescue => e
    report_error("Error fetching pull request: #{e.message}")
  end
end
