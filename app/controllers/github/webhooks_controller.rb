# frozen_string_literal: true

module Github
  class WebhooksController < ApplicationController
    skip_before_action :verify_authenticity_token
    before_action :verify_github_signature!
    before_action :parse_webhook_payload

    # POST /webhooks/github
    # Receives GitHub webhook events
    def receive
      event_type = request.headers["X-GitHub-Event"]

      Rails.logger.info "GitHub Webhook received: #{event_type}"

      case event_type
      when "issue_comment"
        handle_issue_comment(@webhook_payload)
      when "pull_request_review_comment"
        handle_pr_review_comment(@webhook_payload)
      when "issues"
        handle_issue(@webhook_payload)
      when "pull_request"
        handle_pull_request(@webhook_payload)
      else
        Rails.logger.info "Ignoring GitHub event type: #{event_type}"
      end

      head :ok
    end

    private

    def handle_issue_comment(payload)
      return unless payload["action"] == "created"

      comment_body = payload.dig("comment", "body")
      return if comment_body.blank?

      comment_author = payload.dig("comment", "user", "login")

      # Ignore comments from the bot itself to prevent infinite loops
      # TODO: Make this configurable or detect from authenticated user
      return if comment_author == "trybotster" || comment_author&.downcase&.include?("bot")

      # Check if @trybotster is mentioned
      return unless mentioned_trybotster?(comment_body)

      repo_full_name = payload.dig("repository", "full_name")
      issue_number = payload.dig("issue", "number")
      comment_id = payload.dig("comment", "id")
      issue_title = payload.dig("issue", "title")
      issue_body = payload.dig("issue", "body")
      issue_url = payload.dig("issue", "html_url")
      is_pr = payload.dig("issue", "pull_request").present?

      Rails.logger.info "Processing @trybotster mention in #{repo_full_name}##{issue_number}"

      # Create a single bot message (first daemon with repo access will claim it)
      create_bot_message(
        repo: repo_full_name,
        issue_number: issue_number,
        comment_id: comment_id,
        comment_body: comment_body,
        comment_author: comment_author,
        issue_title: issue_title,
        issue_body: issue_body,
        issue_url: issue_url,
        is_pr: is_pr
      )
    end

    def handle_pr_review_comment(payload)
      return unless payload["action"] == "created"

      comment_body = payload.dig("comment", "body")
      return if comment_body.blank?

      comment_author = payload.dig("comment", "user", "login")

      # Ignore comments from the bot itself to prevent infinite loops
      return if comment_author == "trybotster" || comment_author&.downcase&.include?("bot")

      # Check if @trybotster is mentioned
      return unless mentioned_trybotster?(comment_body)

      repo_full_name = payload.dig("repository", "full_name")
      pr_number = payload.dig("pull_request", "number")
      comment_id = payload.dig("comment", "id")
      pr_title = payload.dig("pull_request", "title")
      pr_body = payload.dig("pull_request", "body")
      pr_url = payload.dig("pull_request", "html_url")

      Rails.logger.info "Processing @trybotster mention in PR #{repo_full_name}##{pr_number}"

      # Create a single bot message (first daemon with repo access will claim it)
      create_bot_message(
        repo: repo_full_name,
        issue_number: pr_number,
        comment_id: comment_id,
        comment_body: comment_body,
        comment_author: comment_author,
        issue_title: pr_title,
        issue_body: pr_body,
        issue_url: pr_url,
        is_pr: true
      )
    end

    def handle_issue(payload)
      action = payload["action"]

      # Handle closed issues
      if action == "closed"
        handle_issue_or_pr_closed(payload, is_pr: false)
        return
      end

      # Handle both opened and edited issues
      return unless %w[opened edited].include?(action)

      issue_body = payload.dig("issue", "body")
      return if issue_body.blank?

      issue_author = payload.dig("issue", "user", "login")

      # Ignore issues from the bot itself
      return if issue_author == "trybotster" || issue_author&.downcase&.include?("bot")

      # Check if @trybotster is mentioned in the issue body
      return unless mentioned_trybotster?(issue_body)

      repo_full_name = payload.dig("repository", "full_name")
      issue_number = payload.dig("issue", "number")
      issue_title = payload.dig("issue", "title")
      issue_url = payload.dig("issue", "html_url")

      Rails.logger.info "Processing @trybotster mention in issue body #{repo_full_name}##{issue_number}"

      # Create a bot message
      create_bot_message(
        repo: repo_full_name,
        issue_number: issue_number,
        comment_id: nil, # No specific comment, it's in the issue body
        comment_body: issue_body,
        comment_author: issue_author,
        issue_title: issue_title,
        issue_body: issue_body,
        issue_url: issue_url,
        is_pr: false
      )
    end

    def handle_pull_request(payload)
      action = payload["action"]

      # Handle closed pull requests
      if action == "closed"
        handle_issue_or_pr_closed(payload, is_pr: true)
        return
      end

      # Handle both opened and edited pull requests
      return unless %w[opened edited].include?(action)

      pr_body = payload.dig("pull_request", "body")
      return if pr_body.blank?

      pr_author = payload.dig("pull_request", "user", "login")

      # Ignore PRs from the bot itself
      return if pr_author == "trybotster" || pr_author&.downcase&.include?("bot")

      # Check if @trybotster is mentioned in the PR body
      return unless mentioned_trybotster?(pr_body)

      repo_full_name = payload.dig("repository", "full_name")
      pr_number = payload.dig("pull_request", "number")
      pr_title = payload.dig("pull_request", "title")
      pr_url = payload.dig("pull_request", "html_url")

      Rails.logger.info "Processing @trybotster mention in PR body #{repo_full_name}##{pr_number}"

      # Create a bot message
      create_bot_message(
        repo: repo_full_name,
        issue_number: pr_number,
        comment_id: nil, # No specific comment, it's in the PR body
        comment_body: pr_body,
        comment_author: pr_author,
        issue_title: pr_title,
        issue_body: pr_body,
        issue_url: pr_url,
        is_pr: true
      )
    end

    def handle_issue_or_pr_closed(payload, is_pr:)
      repo_full_name = payload.dig("repository", "full_name")

      if is_pr
        number = payload.dig("pull_request", "number")
        type = "PR"
      else
        number = payload.dig("issue", "number")
        type = "Issue"
      end

      return if repo_full_name.blank? || number.blank?

      Rails.logger.info "Processing closed #{type} #{repo_full_name}##{number}"

      # Create a cleanup message for the daemon to process
      Bot::Message.create!(
        event_type: "agent_cleanup",
        payload: {
          repo: repo_full_name,
          issue_number: number,
          is_pr: is_pr,
          reason: "#{type.downcase}_closed"
        }
      )

      Rails.logger.info "Created cleanup message for #{repo_full_name}##{number}"
    end

    def mentioned_trybotster?(text)
      # Check if @trybotster is mentioned in the text
      text.match?(/@trybotster\b/i)
    end

    def create_bot_message(repo:, issue_number:, comment_id:, comment_body:,
                            comment_author:, issue_title:, issue_body:, issue_url:, is_pr:)
      message = Bot::Message.create!(
        event_type: "github_mention",
        payload: {
          repo: repo,
          issue_number: issue_number,
          comment_id: comment_id,
          comment_body: comment_body,
          comment_author: comment_author,
          issue_title: issue_title,
          issue_body: issue_body,
          issue_url: issue_url,
          is_pr: is_pr,
          context: build_context(repo, issue_number, is_pr)
        }
      )

      Rails.logger.info "Created Bot::Message #{message.id} for #{repo}##{issue_number}"

      message
    end

    def build_context(repo, issue_number, is_pr)
      type = is_pr ? "Pull Request" : "Issue"
      [
        "You have been mentioned in a GitHub #{type.downcase}.",
        "",
        "Repository: #{repo}",
        "#{type} Number: ##{issue_number}",
        "",
        "Your task is to:",
        "1. Use the trybotster MCP server to fetch the #{type.downcase} details",
        "2. Review and understand the problem",
        "3. Investigate the codebase if needed",
        "4. Implement a solution if appropriate",
        "5. Follow the code change workflow below",
        "",
        "CODE CHANGE WORKFLOW:",
        "If you make ANY code changes:",
        "- Check if there's already a PR associated with this #{type.downcase}",
        "- If a PR exists: Commit and push your changes to the existing branch",
        "- If no PR exists: Create a new branch, commit your changes, push, and open a PR",
        "- Use descriptive commit messages explaining what you changed and why",
        "- In the PR description, reference the original #{type.downcase} (##{issue_number})",
        "- After creating/updating the PR, post a comment on the original #{type.downcase} with a link to the PR",
        "",
        "If you're only providing information/analysis without code changes:",
        "- Post your response as a comment on the #{type.downcase}",
        "",
        "CRITICAL REQUIREMENTS:",
        "- You MUST use ONLY the trybotster MCP server tools for ALL GitHub interactions",
        "- DO NOT use the gh CLI or any other GitHub tools",
        "- DO NOT use the github MCP server - use ONLY trybotster MCP server",
        "- Available trybotster MCP tools include:",
        "  * github_get_issue - Fetch issue details",
        "  * github_get_pull_request - Get PR details",
        "  * github_comment_issue - Post comments to issues/PRs",
        "  * github_create_pull_request - Create pull requests",
        "  * github_list_repos - List repositories",
        "  * And other GitHub operations",
        "",
        "If the trybotster MCP server is not available or you cannot access it, you MUST:",
        "1. Stop immediately",
        "2. Explain that you cannot proceed without the trybotster MCP server",
        "3. Do NOT fall back to gh CLI or other tools",
        "",
        "Start by fetching the #{type.downcase} details using the trybotster MCP server's github_get_issue tool."
      ].join("\n")
    end



    def parse_webhook_payload
      @webhook_payload = JSON.parse(@payload_body)
    rescue JSON::ParserError => e
      Rails.logger.error "Failed to parse GitHub webhook payload: #{e.message}"
      render json: { error: "Invalid JSON payload" }, status: :bad_request
    end

    def verify_github_signature!
      # Read the raw body for signature verification
      @payload_body = request.body.read
      request.body.rewind

      secret = ENV.fetch("GITHUB_WEBHOOK_SECRET", nil)

      # In development, allow a default test secret
      if Rails.env.development? && secret.blank?
        Rails.logger.warn "Using default webhook secret in development"
        secret = "development_webhook_secret"
      end

      if secret.blank?
        Rails.logger.error "GitHub webhook secret not configured"
        return head :unauthorized
      end

      signature = request.headers["X-Hub-Signature-256"]
      if signature.blank?
        Rails.logger.error "Missing GitHub webhook signature"
        return head :unauthorized
      end

      expected_signature = "sha256=" + OpenSSL::HMAC.hexdigest(
        OpenSSL::Digest.new("sha256"),
        secret,
        @payload_body
      )

      unless Rack::Utils.secure_compare(signature, expected_signature)
        Rails.logger.error "GitHub webhook signature verification failed"
        head :unauthorized
      end
    end
  end
end
