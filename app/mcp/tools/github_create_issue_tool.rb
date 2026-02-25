# frozen_string_literal: true

class GithubCreateIssueTool < ApplicationMCPTool
  tool_name "github_create_issue"
  description "Create a new issue in a GitHub repository. Requires the repository in 'owner/repo' format, a title, and optional body text."

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :title, type: "string", description: "Issue title", required: true
  property :body, type: "string", description: "Issue description/body (optional)", required: false
  property :labels, type: "string", description: "Comma-separated list of labels (optional)", required: false

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :title, presence: true, length: { minimum: 1, maximum: 256 }

  def perform
    installation_id = ::Github::App.installation_id_for_repo(repo)
    unless installation_id
      report_error("GitHub App is not installed on #{repo}")
      return
    end

    client = ::Github::App.installation_client(installation_id)

    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Creating issue in #{repo} as [bot] (#{client_info})...")

    # Parse labels if provided
    label_array = labels.present? ? labels.split(",").map(&:strip) : []

    # Add attribution footer to issue body
    enhanced_body = body.to_s + attribution_footer

    # Create issue using installation token (bot attribution)
    issue = client.create_issue(repo, title, enhanced_body, labels: label_array)

    success_message = [
      "✅ Issue created successfully!",
      "",
      "📝 Issue ##{issue[:number]}: #{issue[:title]}",
      "   Repository: #{repo}",
      "   URL: #{issue[:html_url]}",
      "   State: #{issue[:state]}"
    ]

    if issue[:created_at]
      created_time = issue[:created_at].is_a?(String) ? Time.parse(issue[:created_at]) : issue[:created_at]
      success_message << "   Created: #{created_time.strftime('%Y-%m-%d %H:%M')}"
    end

    if issue[:labels]&.any?
      labels_text = issue[:labels].map { |l| l[:name] }.join(", ")
      success_message << "   Labels: #{labels_text}"
    end

    success_message << ""
    success_message << "You can view and edit the issue at: #{issue[:html_url]}"

    render(text: success_message.join("\n"))
  rescue Octokit::Error => e
    report_error("Failed to create issue: #{e.message}")
  rescue => e
    report_error("Error creating issue: #{e.message}")
  end
end
