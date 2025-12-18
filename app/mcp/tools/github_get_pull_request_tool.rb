# frozen_string_literal: true

class GithubGetPullRequestTool < ApplicationMCPTool
  tool_name "github_get_pull_request"
  description "Get detailed information about a specific GitHub pull request, including diff and merge status. Requires repository in 'owner/repo' format and PR number."

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :pr_number, type: "integer", description: "Pull request number", required: true

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
    result = Github::App.get_pull_request(token, repo: repo, pr_number: pr_number.to_i)

    if result[:success]
      pr = result[:pull_request]

      # Handle dates that could be Time objects or strings
      created_at = pr[:created_at].is_a?(String) ? Time.parse(pr[:created_at]) : pr[:created_at]
      updated_at = pr[:updated_at].is_a?(String) ? Time.parse(pr[:updated_at]) : pr[:updated_at]

      pr_details = [
        "🔀 Pull Request ##{pr[:number]}: #{pr[:title]}",
        "=" * 60,
        "",
        "📍 Repository: #{repo}",
        "🔗 URL: #{pr[:html_url]}",
        "📊 State: #{pr[:state]}#{pr[:merged] ? ' (merged)' : ''}#{pr[:draft] ? ' (draft)' : ''}",
        "👤 Author: #{pr[:user][:login]}",
        "💬 Comments: #{pr[:comments]}",
        "📝 Commits: #{pr[:commits]}",
        "➕ Additions: #{pr[:additions]}",
        "➖ Deletions: #{pr[:deletions]}",
        "📁 Changed Files: #{pr[:changed_files]}",
        "",
        "📅 Created: #{created_at.strftime('%Y-%m-%d %H:%M')}",
        "🔄 Updated: #{updated_at.strftime('%Y-%m-%d %H:%M')}"
      ]

      if pr[:merged_at]
        merged_at = pr[:merged_at].is_a?(String) ? Time.parse(pr[:merged_at]) : pr[:merged_at]
        pr_details << "✅ Merged: #{merged_at.strftime('%Y-%m-%d %H:%M')}"
        pr_details << "   Merged by: #{pr[:merged_by][:login]}" if pr[:merged_by]
      elsif pr[:closed_at]
        closed_at = pr[:closed_at].is_a?(String) ? Time.parse(pr[:closed_at]) : pr[:closed_at]
        pr_details << "❌ Closed: #{closed_at.strftime('%Y-%m-%d %H:%M')}"
      end

      if pr[:assignees]&.any?
        assignees = pr[:assignees].map { |a| a[:login] }.join(", ")
        pr_details << "👥 Assignees: #{assignees}"
      end

      if pr[:requested_reviewers]&.any?
        reviewers = pr[:requested_reviewers].map { |r| r[:login] }.join(", ")
        pr_details << "👀 Requested Reviewers: #{reviewers}"
      end

      if pr[:labels]&.any?
        labels = pr[:labels].map { |l| l[:name] }.join(", ")
        pr_details << "🏷️  Labels: #{labels}"
      end

      pr_details << ""
      pr_details << "🔀 Branch Info:"
      pr_details << "   Head: #{pr[:head][:ref]} (#{pr[:head][:repo]&.dig(:full_name) || 'deleted repo'})"
      pr_details << "   Base: #{pr[:base][:ref]} (#{pr[:base][:repo][:full_name]})"

      if pr[:mergeable] != nil
        pr_details << "   Mergeable: #{pr[:mergeable] ? 'Yes' : 'No'}"
      end
      if pr[:mergeable_state]
        pr_details << "   Mergeable State: #{pr[:mergeable_state]}"
      end

      pr_details << ""
      pr_details << "📝 Description:"
      pr_details << "-" * 60

      if pr[:body].present?
        body_preview = pr[:body][0..1000]
        body_preview += "...\n(truncated)" if pr[:body].length > 1000
        pr_details << body_preview
      else
        pr_details << "(No description provided)"
      end

      pr_details << "-" * 60

      pr_details << ""
      pr_details << "📌 Additional PR Details:"
      pr_details << "   Diff URL: #{pr[:diff_url]}"
      pr_details << "   Patch URL: #{pr[:patch_url]}"

      render(text: pr_details.join("\n"))
    else
      report_error("Failed to fetch pull request: #{result[:error]}")
    end
  rescue => e
    report_error("Error fetching pull request: #{e.message}")
  end
end
