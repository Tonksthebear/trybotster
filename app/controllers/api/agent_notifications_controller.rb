# frozen_string_literal: true

module Api
  # Receives terminal notifications (BEL, OSC) from CLI agents
  # and posts GitHub comments to alert users when agents need input
  class AgentNotificationsController < ApplicationController
    include ApiKeyAuthenticatable

    skip_before_action :verify_authenticity_token
    before_action :authenticate_with_api_key!

    # POST /api/agent_notifications
    # CLI sends notification when terminal bell/alert is detected
    # Accepts either:
    # - invocation_url: GitHub URL (issue or PR) to post comment to
    # - repo + issue_number: legacy format
    def create
      notification_type = params[:notification_type]

      # Parse invocation_url if provided, otherwise use legacy params
      if params[:invocation_url].present?
        parsed = parse_github_url(params[:invocation_url])
        if parsed.nil?
          render json: { error: "Invalid invocation_url format" }, status: :unprocessable_entity
          return
        end
        repo = parsed[:repo]
        issue_number = parsed[:number]
      else
        # Legacy fallback
        repo = params[:repo]
        issue_number = params[:issue_number].to_i
      end

      # Validate we have the required params
      if repo.blank? || issue_number.zero?
        render json: { error: "repo and issue_number required (or valid invocation_url)" }, status: :unprocessable_entity
        return
      end

      # Check if user has GitHub authorization
      unless current_api_user.github_app_authorized?
        render json: { error: "GitHub App not authorized" }, status: :unauthorized
        return
      end

      # Get installation for this repo
      access_token = current_api_user.valid_github_app_token
      installation_result = Github::App.get_installation_for_repo(access_token, repo)

      unless installation_result[:success]
        render json: { error: installation_result[:error] }, status: :unprocessable_entity
        return
      end

      # Post comment as bot
      comment_body = build_notification_comment(notification_type)
      result = post_github_comment(installation_result[:installation_id], repo, issue_number, comment_body)

      if result[:success]
        Rails.logger.info "Posted agent notification to #{repo}##{issue_number}: #{notification_type}"
        render json: { success: true, comment_url: result[:comment][:html_url] }, status: :created
      else
        Rails.logger.error "Failed to post notification: #{result[:error]}"
        render json: { error: result[:error] }, status: :unprocessable_entity
      end
    end

    private

    # Parse a GitHub URL to extract repo and issue/PR number
    # Examples:
    #   https://github.com/owner/repo/issues/123
    #   https://github.com/owner/repo/pull/456
    # Returns { repo: "owner/repo", number: 123 } or nil if invalid
    def parse_github_url(url)
      return nil if url.blank?

      # Match GitHub issue or PR URLs
      match = url.match(%r{github\.com/([^/]+/[^/]+)/(issues|pull)/(\d+)})
      return nil unless match

      {
        repo: match[1],
        number: match[3].to_i
      }
    end

    def build_notification_comment(notification_type)
      case notification_type
      when "bell"
        "🔔 **Agent needs your attention!**\n\n" \
        "The agent is waiting for input or approval. " \
        "Please check the terminal session and respond to continue."
      when "question_asked"
        "❓ **Agent is asking a question!**\n\n" \
        "The agent is waiting for your input to continue. " \
        "Please check the terminal session and respond."
      when /^osc9:/
        message = notification_type.sub("osc9:", "").presence
        if message
          "🔔 **Agent notification:**\n\n#{message}"
        else
          "🔔 **Agent needs your attention!**\n\n" \
          "The agent sent a notification. Please check the terminal session."
        end
      when /^osc777:/
        parts = notification_type.sub("osc777:", "").split(":", 2)
        title = parts[0].presence || "Notification"
        body = parts[1].presence || "Agent needs attention"
        "🔔 **#{title}**\n\n#{body}"
      else
        "🔔 **Agent needs your attention!**\n\n" \
        "The agent sent a notification (#{notification_type}). Please check the terminal session."
      end
    end

    def post_github_comment(installation_id, repo, issue_number, body)
      client = Github::App.installation_client(installation_id)
      comment = client.add_comment(repo, issue_number, body)
      { success: true, comment: comment.to_h }
    rescue Octokit::Error => e
      { success: false, error: e.message }
    rescue => e
      { success: false, error: e.message }
    end
  end
end
