# frozen_string_literal: true

class GithubUpdateIssueTool < ApplicationMCPTool
  tool_name "github_update_issue"
  description "Update an existing GitHub issue or pull request. Can update state (open/closed), title, body, labels, and assignees."

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :issue_number, type: "number", description: "Issue or PR number", required: true
  property :state, type: "string", description: "Issue state: 'open' or 'closed' (optional)", required: false
  property :title, type: "string", description: "New issue title (optional)", required: false
  property :body, type: "string", description: "New issue body (optional)", required: false
  property :labels, type: "string", description: "Comma-separated list of labels to set (optional)", required: false
  property :assignees, type: "string", description: "Comma-separated list of GitHub usernames to assign (optional)", required: false

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :issue_number, numericality: { only_integer: true, greater_than: 0 }
  validates :state, inclusion: { in: %w[open closed], message: "must be 'open' or 'closed'" }, allow_nil: true

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
    render(text: "Updating #{repo}##{issue_number} as [bot] (via #{client_info})...")

    # Get installation client (shows as [bot])
    begin
      client = Github::App.installation_client(current_user.github_app_installation_id)
    rescue => e
      report_error("Failed to get bot credentials: #{e.message}")
      return
    end

    # Build update options
    update_options = {}
    update_options[:state] = state if state.present?
    update_options[:title] = title if title.present?
    update_options[:body] = body if body.present?
    update_options[:labels] = labels.split(",").map(&:strip) if labels.present?
    update_options[:assignees] = assignees.split(",").map(&:strip) if assignees.present?

    if update_options.empty?
      report_error("No update parameters provided. Specify at least one of: state, title, body, labels, or assignees")
      return
    end

    # Update issue using installation token (bot attribution)
    begin
      issue = client.update_issue(repo, issue_number.to_i, update_options)
      result = { success: true, issue: issue.to_h }
    rescue Octokit::Error => e
      Rails.logger.error "Octokit error: #{e.message}"
      result = { success: false, error: e.message }
    rescue => e
      Rails.logger.error "General error updating issue: #{e.message}"
      result = { success: false, error: e.message }
    end

    if result[:success]
      issue = result[:issue]
      is_pr = issue[:pull_request].present?
      icon = is_pr ? "🔀" : "📝"

      success_message = [
        "✅ #{is_pr ? 'Pull request' : 'Issue'} updated successfully!",
        "",
        "#{icon} ##{issue[:number]}: #{issue[:title]}",
        "   Repository: #{repo}",
        "   URL: #{issue[:html_url]}",
        "   State: #{issue[:state]}"
      ]

      if issue[:labels]&.any?
        labels_text = issue[:labels].map { |l| l[:name] }.join(", ")
        success_message << "   Labels: #{labels_text}"
      end

      if issue[:assignees]&.any?
        assignees_text = issue[:assignees].map { |a| a[:login] }.join(", ")
        success_message << "   Assignees: #{assignees_text}"
      end

      if issue[:updated_at]
        updated_time = issue[:updated_at].is_a?(String) ? Time.parse(issue[:updated_at]) : issue[:updated_at]
        success_message << "   Updated: #{updated_time.strftime('%Y-%m-%d %H:%M')}"
      end

      render(text: success_message.join("\n"))
    else
      report_error("Failed to update issue: #{result[:error]}")
    end
  rescue => e
    report_error("Error updating issue: #{e.message}")
  end
end
