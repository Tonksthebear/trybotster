# frozen_string_literal: true

class GithubCreatePullRequestReviewTool < ApplicationMCPTool
  tool_name "github_create_pull_request_review"
  description <<~DESC
    Submit a review on a GitHub pull request with optional inline code comments.

    RECOMMENDED WORKFLOW:
    1. Call github_get_pull_request_files — returns each changed file with its diff patch,
       annotated with right-side line numbers you can use directly as `line` values.
    2. Call github_get_pull_request to read the PR title, description, and metadata.
    3. Call this tool with event + body + comments (using line numbers from step 1).

    IMPORTANT CONSTRAINTS:
    - Inline comment `line` values MUST refer to lines visible in the PR diff. Commenting on
      a line outside the diff causes a 422 error. Always read the diff before picking line numbers.
    - The bot cannot APPROVE or REQUEST_CHANGES on a PR it authored. Use COMMENT in that case.
    - REQUEST_CHANGES requires a non-empty `body`.

    You can submit a review with only a top-level `body` (no inline comments), only inline
    `comments` (no body), or both together.
  DESC

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :pr_number, type: "integer", description: "Pull request number", required: true
  property :event, type: "string", description: "Review verdict: 'APPROVE', 'REQUEST_CHANGES', or 'COMMENT'. Use COMMENT when you are the PR author — GitHub prevents authors from approving or requesting changes on their own PRs.", required: true
  property :body, type: "string", description: "Top-level review summary (required for REQUEST_CHANGES, recommended for all events). Supports markdown.", required: false
  property :comments, type: "array", description: "Inline code comments attached to specific diff lines. Each item must have: path (file path relative to repo root, e.g. 'src/foo.rb'), line (line number as it appears in the diff — must be a line visible in the diff, not an arbitrary file line), body (comment text, supports markdown). Example: [{\"path\": \"src/foo.rb\", \"line\": 42, \"body\": \"This could be simplified to X\"}]", required: false

  # ActionMCP maps type:"array" to :string, which casts via Array#to_s (inspect format,
  # not JSON). Override the accessors to preserve the raw Array/value for correct behaviour.
  def comments=(value)
    @_raw_comments = value
  end

  def comments
    @_raw_comments
  end

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :pr_number, numericality: { only_integer: true, greater_than: 0 }
  validates :event, inclusion: { in: %w[APPROVE REQUEST_CHANGES COMMENT], message: "must be 'APPROVE', 'REQUEST_CHANGES', or 'COMMENT'" }
  validate :body_required_for_request_changes
  validate :comments_structure

  def perform
    # Prevent duplicate review submissions on agent retries
    cached_response = check_idempotency_cache
    if cached_response
      render(text: cached_response["text"])
      return
    end

    installation_id = ::Github::App.installation_id_for_repo(repo)
    unless installation_id
      report_error("GitHub App is not installed on #{repo}")
      return
    end

    client = ::Github::App.installation_client(installation_id)
    client_info = detect_client_type

    review_options = { event: event }
    review_options[:body] = body.to_s + attribution_footer if body.present?

    parsed = parsed_comments
    if parsed.present?
      review_options[:comments] = parsed.map do |c|
        { path: c["path"] || c[:path], line: (c["line"] || c[:line]).to_i, body: c["body"] || c[:body] }
      end
    end

    review = client.create_pull_request_review(repo, pr_number.to_i, **review_options)

    event_label = case event
    when "APPROVE"          then "✅ Approved"
    when "REQUEST_CHANGES"  then "🔄 Changes requested"
    when "COMMENT"          then "💬 Reviewed"
    end

    inline_count = Array(parsed).count

    output = [
      "#{event_label} — #{repo}##{pr_number}",
      "",
      "🔗 Review URL: #{review[:html_url]}",
      "   Submitted via: #{client_info}"
    ]

    if inline_count > 0
      output << "   Inline comments: #{inline_count}"
    end

    if body.present?
      output << ""
      output << "📝 Review summary:"
      output << body
    end

    response_text = output.join("\n")
    store_idempotency_response(response_text)
    render(text: response_text)
  rescue Octokit::UnprocessableEntity => e
    report_error("Failed to submit review — #{e.message}. Ensure the PR is open and all comment line numbers refer to lines visible in the diff.")
  rescue Octokit::Error => e
    report_error("Failed to submit review: #{e.message}")
  rescue => e
    report_error("Error submitting review: #{e.message}")
  end

  private

  def body_required_for_request_changes
    if event == "REQUEST_CHANGES" && body.blank?
      errors.add(:body, "is required when requesting changes")
    end
  end

  def parsed_comments
    return nil if comments.blank?
    return comments if comments.is_a?(Array)

    JSON.parse(comments)
  rescue JSON::ParserError
    nil
  end

  def comments_structure
    return if comments.blank?

    parsed = parsed_comments
    unless parsed.is_a?(Array)
      errors.add(:comments, "must be an array")
      return
    end

    parsed.each_with_index do |c, i|
      path = c["path"] || c[:path]
      line = c["line"] || c[:line]
      cbody = c["body"] || c[:body]

      errors.add(:comments, "item #{i + 1} is missing 'path'") if path.blank?
      errors.add(:comments, "item #{i + 1} is missing 'line'") if line.nil?
      errors.add(:comments, "item #{i + 1} is missing 'body'") if cbody.blank?
      errors.add(:comments, "item #{i + 1} 'line' must be a positive integer") if line.present? && line.to_i < 1
    end
  end
end
