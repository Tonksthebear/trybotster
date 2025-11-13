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
    render(text: "Creating issue in #{repo} as [bot] (via #{client_info})...")

    # Parse labels if provided
    label_array = labels.present? ? labels.split(',').map(&:strip) : []

    # Get installation client (shows as [bot])
    begin
      client = Github::App.installation_client(current_user.github_app_installation_id)
    rescue => e
      report_error("Failed to get bot credentials: #{e.message}")
      return
    end

    # Add attribution footer to issue body
    enhanced_body = body.to_s + attribution_footer

    # Create issue using installation token (bot attribution)
    begin
      issue = client.create_issue(repo, title, enhanced_body, labels: label_array)
      result = { success: true, issue: issue.to_h }
    rescue Octokit::Error => e
      Rails.logger.error "Octokit error: #{e.message}"
      result = { success: false, error: e.message }
    rescue => e
      Rails.logger.error "General error creating issue: #{e.message}"
      result = { success: false, error: e.message }
    end

    if result[:success]
      issue = result[:issue]

      success_message = [
        "✅ Issue created successfully!",
        "",
        "📝 Issue ##{issue[:number]}: #{issue[:title]}",
        "   Repository: #{repo}",
        "   URL: #{issue[:html_url]}",
        "   State: #{issue[:state]}"
      ]

      # Add created_at only if it exists
      if issue[:created_at]
        created_time = issue[:created_at].is_a?(String) ? Time.parse(issue[:created_at]) : issue[:created_at]
        success_message << "   Created: #{created_time.strftime('%Y-%m-%d %H:%M')}"
      end

      if issue[:labels]&.any?
        labels_text = issue[:labels].map { |l| l[:name] }.join(', ')
        success_message << "   Labels: #{labels_text}"
      end

      success_message << ""
      success_message << "You can view and edit the issue at: #{issue[:html_url]}"

      render(text: success_message.join("\n"))
    else
      report_error("Failed to create issue: #{result[:error]}")
    end
  rescue => e
    report_error("Error creating issue: #{e.message}")
  end
end
