# frozen_string_literal: true

module Github
  class WebhooksController < ApplicationController
    skip_before_action :verify_authenticity_token
    before_action :verify_github_signature!
    before_action :parse_webhook_payload

    # Event type to handler class mapping
    HANDLERS = {
      "issue_comment" => Integrations::Github::Webhooks::IssueCommentHandler,
      "pull_request_review_comment" => Integrations::Github::Webhooks::PrReviewCommentHandler,
      "issues" => Integrations::Github::Webhooks::IssueHandler,
      "pull_request" => Integrations::Github::Webhooks::PullRequestHandler
    }.freeze

    # POST /webhooks/github
    # Receives GitHub webhook events
    def receive
      event_type = request.headers["X-GitHub-Event"]

      Rails.logger.info "GitHub Webhook received: #{event_type}"

      handler_class = HANDLERS[event_type]

      if handler_class
        handler_class.new(@webhook_payload).call
      else
        Rails.logger.info "Ignoring GitHub event type: #{event_type}"
      end

      head :ok
    end

    private

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
