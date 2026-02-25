# frozen_string_literal: true

class GithubCreatePullRequestTool < ApplicationMCPTool
  tool_name "github_create_pull_request"
  description "Create a new pull request in a GitHub repository. Requires repository in 'owner/repo' format, title, head branch (source), and base branch (target, usually 'main')."

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :title, type: "string", description: "Pull request title", required: true
  property :body, type: "string", description: "Pull request description/body (optional)", required: false
  property :head, type: "string", description: "The name of the branch where your changes are (e.g., 'feature-branch')", required: true
  property :base, type: "string", description: "The name of the branch you want to merge into (e.g., 'main')", required: true
  property :draft, type: "boolean", description: "Create as draft PR (default: false)", required: false

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :title, presence: true, length: { minimum: 1, maximum: 256 }
  validates :head, presence: true
  validates :base, presence: true

  def perform
    installation_id = ::Github::App.installation_id_for_repo(repo)
    unless installation_id
      report_error("GitHub App is not installed on #{repo}")
      return
    end

    client = ::Github::App.installation_client(installation_id)

    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Creating pull request in #{repo} as [bot] (#{client_info})...")

    # Add attribution footer to PR body
    enhanced_body = body.to_s + attribution_footer

    # Create PR using installation token (bot attribution)
    pr = client.create_pull_request(repo, base, head, title, enhanced_body, draft: draft || false)

    success_message = [
      "✅ Pull request created successfully!",
      "",
      "🔀 PR ##{pr[:number]}: #{pr[:title]}",
      "   Repository: #{repo}",
      "   URL: #{pr[:html_url]}",
      "   State: #{pr[:state]}",
      "   Draft: #{pr[:draft] ? 'Yes' : 'No'}",
      "   #{pr[:head][:ref]} → #{pr[:base][:ref]}"
    ]

    if pr[:created_at]
      created_time = pr[:created_at].is_a?(String) ? Time.parse(pr[:created_at]) : pr[:created_at]
      success_message << "   Created: #{created_time.strftime('%Y-%m-%d %H:%M')}"
    end

    success_message << ""
    success_message << "You can view and edit the PR at: #{pr[:html_url]}"

    render(text: success_message.join("\n"))
  rescue Octokit::UnprocessableEntity => e
    if e.message.include?("not all refs are readable")
      error_message = [
        "❌ Cannot access branches in #{repo}. Possible causes:",
        "",
        "1. Branch '#{head}' doesn't exist on GitHub",
        "   → Push it with: git push origin #{head}",
        "",
        "2. Branch '#{base}' doesn't exist",
        "   → Verify the base branch name is correct",
        "",
        "3. GitHub App doesn't have access to #{repo}",
        "   → Check installation settings at: https://github.com/settings/installations",
        "",
        "Full error: #{e.message}"
      ].join("\n")

      report_error("Failed to create pull request: #{error_message}")
    else
      report_error("Failed to create pull request: #{e.message}")
    end
  rescue Octokit::Error => e
    report_error("Failed to create pull request: #{e.message}")
  rescue => e
    report_error("Error creating pull request: #{e.message}")
  end
end
