# frozen_string_literal: true

class GithubGetIssueTool < ApplicationMCPTool
  tool_name "github_get_issue"
  description "Get detailed information about a specific GitHub issue or pull request. Requires the repository in 'owner/repo' format and the issue number."

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

    # Identify the Botster session for user feedback
    agent_info = botster_session_attribution
    render(text: "Fetching details for #{repo}##{issue_number} (via #{agent_info})...")

    issue = client.issue(repo, issue_number.to_i)

    # Determine if it's a PR or issue
    is_pr = issue[:pull_request].present?
    icon = is_pr ? "🔀 Pull Request" : "🐛 Issue"

    issue_details = [
      "#{icon} ##{issue[:number]}: #{issue[:title]}",
      "=" * 60,
      "",
      "📍 Repository: #{repo}",
      "🔗 URL: #{issue[:html_url]}",
      "📊 State: #{issue[:state]}",
      "👤 Author: #{issue[:user][:login]}",
      "💬 Comments: #{issue[:comments]}",
      "",
      "📅 Created: #{issue[:created_at]&.strftime('%Y-%m-%d %H:%M') || 'Unknown'}",
      "🔄 Updated: #{issue[:updated_at]&.strftime('%Y-%m-%d %H:%M') || 'Unknown'}"
    ]

    if issue[:closed_at]
      issue_details << "❌ Closed: #{issue[:closed_at]&.strftime('%Y-%m-%d %H:%M') || 'Unknown'}"
    end

    if issue[:assignees]&.any?
      assignees = issue[:assignees].map { |a| a[:login] }.join(", ")
      issue_details << "👥 Assignees: #{assignees}"
    end

    if issue[:labels]&.any?
      labels = issue[:labels].map { |l| l[:name] }.join(", ")
      issue_details << "🏷️  Labels: #{labels}"
    end

    if issue[:milestone]
      issue_details << "🎯 Milestone: #{issue[:milestone][:title]}"
    end

    issue_details << ""
    issue_details << "📝 Description:"
    issue_details << "-" * 60

    if issue[:body].present?
      # Show first 1000 characters of body
      body_preview = issue[:body][0..1000]
      body_preview += "...\n(truncated)" if issue[:body].length > 1000
      issue_details << body_preview
    else
      issue_details << "(No description provided)"
    end

    issue_details << "-" * 60

    if is_pr && issue[:pull_request]
      issue_details << ""
      issue_details << "📌 Pull Request Details:"
      pr_info = issue[:pull_request]
      issue_details << "   Mergeable: #{pr_info[:merged_at] ? 'Merged' : 'Open'}"
      issue_details << "   Diff URL: #{pr_info[:diff_url]}" if pr_info[:diff_url]
      issue_details << "   Patch URL: #{pr_info[:patch_url]}" if pr_info[:patch_url]
    end

    render(text: issue_details.join("\n"))
  rescue Octokit::Error => e
    report_error("Failed to fetch issue: #{e.message}")
  rescue => e
    report_error("Error fetching issue: #{e.message}")
  end
end
