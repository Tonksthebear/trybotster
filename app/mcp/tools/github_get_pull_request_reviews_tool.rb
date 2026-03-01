# frozen_string_literal: true

class GithubGetPullRequestReviewsTool < ApplicationMCPTool
  tool_name "github_get_pull_request_reviews"
  description <<~DESC
    Get all reviews and inline code comments on a GitHub pull request.

    GitHub PRs have two distinct comment types — this tool fetches both:

    1. REVIEWS — top-level review submissions (APPROVE / REQUEST_CHANGES / COMMENT)
       with their summary body. Fetched via the Reviews API.

    2. INLINE COMMENTS — line-level comments on specific diff lines, attached to
       a review. Fetched via the Pull Request Review Comments API.

    Note: this tool does NOT return general issue/conversation comments (the timeline
    comments posted directly on the PR). Use github_get_issue_comments for those.
  DESC

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :pr_number, type: "integer", description: "Pull request number", required: true

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :pr_number, numericality: { only_integer: true, greater_than: 0 }

  def perform
    installation_id = ::Github::App.installation_id_for_repo(repo)
    unless installation_id
      report_error("GitHub App is not installed on #{repo}")
      return
    end

    client = ::Github::App.installation_client(installation_id)

    reviews  = client.pull_request_reviews(repo, pr_number.to_i)
    comments = client.pull_request_comments(repo, pr_number.to_i)

    if reviews.empty? && comments.empty?
      render(text: "No reviews or inline comments found on #{repo}##{pr_number}")
      return
    end

    # Index inline comments by review_id for grouping
    comments_by_review = comments.group_by { |c| c[:pull_request_review_id] }

    output = [
      "🔍 Reviews on #{repo}##{pr_number}",
      "=" * 80,
      ""
    ]

    if reviews.any?
      reviews.each_with_index do |review, index|
        event_label = case review[:state]
        when "APPROVED"          then "✅ APPROVED"
        when "CHANGES_REQUESTED" then "🔄 CHANGES REQUESTED"
        when "COMMENTED"         then "💬 COMMENT"
        when "DISMISSED"         then "🚫 DISMISSED"
        else                          review[:state]
        end

        author     = review[:user][:login]
        submitted  = review[:submitted_at]&.strftime("%Y-%m-%d %H:%M") || "Unknown"
        review_id  = review[:id]

        output << "#{index + 1}. #{event_label} by #{author} • #{submitted}"
        output << "   🔗 #{review[:html_url]}"

        if review[:body].present?
          output << ""
          output << "   Summary:"
          review[:body].lines.each { |l| output << "   #{l.rstrip}" }
        end

        inline = comments_by_review[review_id]
        if inline&.any?
          output << ""
          output << "   Inline comments (#{inline.count}):"
          inline.each do |c|
            output << ""
            output << "   📄 #{c[:path]}:#{c[:line] || c[:original_line]}"
            output << "   👤 #{c[:user][:login]} • #{c[:created_at]&.strftime("%Y-%m-%d %H:%M")}"
            output << "   🔗 #{c[:html_url]}"
            output << ""
            c[:body].to_s.lines.each { |l| output << "      #{l.rstrip}" }
          end
        end

        output << ""
        output << "-" * 80
        output << ""
      end
    end

    # Orphaned inline comments (not associated with a formal review)
    orphaned = comments_by_review[nil]
    if orphaned&.any?
      output << "💬 Standalone inline comments (#{orphaned.count}):"
      output << ""
      orphaned.each do |c|
        output << "📄 #{c[:path]}:#{c[:line] || c[:original_line]}"
        output << "👤 #{c[:user][:login]} • #{c[:created_at]&.strftime("%Y-%m-%d %H:%M")}"
        output << "🔗 #{c[:html_url]}"
        output << ""
        c[:body].to_s.lines.each { |l| output << "   #{l.rstrip}" }
        output << ""
      end
    end

    summary_parts = []
    summary_parts << "#{reviews.count} review(s)" if reviews.any?
    summary_parts << "#{comments.count} inline comment(s)" if comments.any?
    output << "Total: #{summary_parts.join(", ")}"

    render(text: output.join("\n"))
  rescue Octokit::Error => e
    report_error("Failed to fetch pull request reviews: #{e.message}")
  rescue => e
    report_error("Error fetching pull request reviews: #{e.message}")
  end
end
