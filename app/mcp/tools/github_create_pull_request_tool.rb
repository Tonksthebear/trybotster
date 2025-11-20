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
    # Check if user has authorized GitHub App
    unless current_user&.github_app_authorized?
      report_error("GitHub App not authorized. Please authorize at /github_app/authorize")
      return
    end

    # Get the correct installation ID for this repository
    access_token = current_user.valid_github_app_token
    installation_result = Github::App.get_installation_for_repo(access_token, repo)

    unless installation_result[:success]
      report_error(installation_result[:error])
      return
    end

    installation_id = installation_result[:installation_id]
    Rails.logger.info "Using GitHub App installation ID: #{installation_id} for #{repo} (account: #{installation_result[:account]})"

    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Creating pull request in #{repo} as [bot] via #{installation_result[:account]} installation (#{client_info})...")

    # Get installation client (shows as [bot])
    begin
      client = Github::App.installation_client(installation_id)
    rescue => e
      report_error("Failed to get bot credentials: #{e.message}")
      return
    end

    # Add attribution footer to PR body
    enhanced_body = body.to_s + attribution_footer

    # Create PR using installation token (bot attribution)
    begin
      pr_options = {
        body: enhanced_body,
        draft: draft || false
      }

      pr = client.create_pull_request(repo, base, head, title, pr_options[:body], draft: pr_options[:draft])
      result = { success: true, pr: pr.to_h }
    rescue Octokit::UnprocessableEntity => e
      # This typically means branch doesn't exist or GitHub App lacks permissions
      if e.message.include?("not all refs are readable")
        Rails.logger.error "Refs not readable: #{e.message}"

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
          "   → Ensure the app is installed for this repository",
          "",
          "Full error: #{e.message}"
        ].join("\n")

        result = { success: false, error: error_message }
      else
        Rails.logger.error "Octokit unprocessable entity: #{e.message}"
        result = { success: false, error: e.message }
      end
    rescue Octokit::Error => e
      Rails.logger.error "Octokit error: #{e.message}"
      result = { success: false, error: e.message }
    rescue => e
      Rails.logger.error "General error creating PR: #{e.message}"
      result = { success: false, error: e.message }
    end

    if result[:success]
      pr = result[:pr]

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
    else
      report_error("Failed to create pull request: #{result[:error]}")
    end
  rescue => e
    report_error("Error creating pull request: #{e.message}")
  end
end
