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

      # Find all @username mentions in the comment
      mentioned_usernames = extract_mentions(comment_body)
      return if mentioned_usernames.empty?

      repo_full_name = payload.dig("repository", "full_name")
      issue_number = payload.dig("issue", "number")
      comment_id = payload.dig("comment", "id")
      comment_author = payload.dig("comment", "user", "login")
      issue_title = payload.dig("issue", "title")
      issue_body = payload.dig("issue", "body")
      issue_url = payload.dig("issue", "html_url")
      is_pr = payload.dig("issue", "pull_request").present?

      Rails.logger.info "Processing mentions: #{mentioned_usernames.join(', ')} in #{repo_full_name}##{issue_number}"

      # Find users by GitHub username and create bot messages
      mentioned_usernames.each do |username|
        user = find_user_by_github_username(username)

        if user
          create_bot_message_for_user(
            user: user,
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
        else
          Rails.logger.warn "User not found for GitHub username: #{username}"
        end
      end
    end

    def handle_pr_review_comment(payload)
      return unless payload["action"] == "created"

      comment_body = payload.dig("comment", "body")
      return if comment_body.blank?

      mentioned_usernames = extract_mentions(comment_body)
      return if mentioned_usernames.empty?

      repo_full_name = payload.dig("repository", "full_name")
      pr_number = payload.dig("pull_request", "number")
      comment_id = payload.dig("comment", "id")
      comment_author = payload.dig("comment", "user", "login")
      pr_title = payload.dig("pull_request", "title")
      pr_body = payload.dig("pull_request", "body")
      pr_url = payload.dig("pull_request", "html_url")

      Rails.logger.info "Processing PR review mentions: #{mentioned_usernames.join(', ')} in #{repo_full_name}##{pr_number}"

      mentioned_usernames.each do |username|
        user = find_user_by_github_username(username)

        if user
          create_bot_message_for_user(
            user: user,
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
        else
          Rails.logger.warn "User not found for GitHub username: #{username}"
        end
      end
    end

    def extract_mentions(text)
      # Extract @username mentions (GitHub format)
      text.scan(/@([\w-]+)/).flatten.uniq
    end

    def find_user_by_github_username(username)
      # Find user by their GitHub username
      User.find_by(username: username)
    end

    def create_bot_message_for_user(user:, repo:, issue_number:, comment_id:, comment_body:,
                                     comment_author:, issue_title:, issue_body:, issue_url:, is_pr:)
      message = user.bot_messages.create!(
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
          context: build_context(issue_title, issue_body, comment_body, is_pr)
        }
      )

      Rails.logger.info "Created Bot::Message #{message.id} for user #{user.id} (#{user.username})"

      # Broadcast to user's WebSocket channel if connected
      broadcast_to_user(user, message)
    end

    def build_context(issue_title, issue_body, comment_body, is_pr)
      type = is_pr ? "Pull Request" : "Issue"
      [
        "#{type}: #{issue_title}",
        "",
        "Description:",
        issue_body.to_s[0..500], # First 500 chars
        "",
        "Comment:",
        comment_body.to_s[0..500]
      ].join("\n")
    end

    def broadcast_to_user(user, message)
      # Broadcast via ActionCable
      BotChannel.broadcast_to(
        user,
        {
          type: "new_message",
          message_id: message.id,
          event_type: message.event_type,
          payload: message.payload,
          created_at: message.created_at
        }
      )
    rescue => e
      Rails.logger.error "Failed to broadcast message to user #{user.id}: #{e.message}"
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
