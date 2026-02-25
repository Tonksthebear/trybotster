# frozen_string_literal: true

class GithubListIssuesTool < ApplicationMCPTool
  tool_name "github_list_issues"
  description "List GitHub issues for a repository. Can filter by state: open, closed, or all."

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :state, type: "string", description: "State: open, closed, all (default: open)", required: false

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }

  def perform
    installation_id = ::Github::App.installation_id_for_repo(repo)
    unless installation_id
      report_error("GitHub App is not installed on #{repo}")
      return
    end

    client = ::Github::App.installation_client(installation_id)
    state_param = state || "open"

    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Fetching #{state_param} issues for #{repo} (via #{client_info})...")

    issues = client.list_issues(repo, state: state_param)

    if issues.empty?
      render(text: "No issues found in #{repo} with state '#{state_param}'.")
      return
    end

    render(text: "Found #{issues.count} issues:\n")

    issues.each do |issue|
      # Determine if it's a PR or issue
      is_pr = issue[:pull_request].present?
      icon = is_pr ? "🔀" : "🐛"

      created_at = issue[:created_at].is_a?(String) ? Time.parse(issue[:created_at]) : issue[:created_at]
      updated_at = issue[:updated_at].is_a?(String) ? Time.parse(issue[:updated_at]) : issue[:updated_at]

      issue_info = [
        "#{icon} ##{issue[:number]}: #{issue[:title]}",
        "   State: #{issue[:state]} | Comments: #{issue[:comments]}",
        "   URL: #{issue[:html_url]}",
        "   Created: #{created_at.strftime('%Y-%m-%d %H:%M')}",
        "   Updated: #{updated_at.strftime('%Y-%m-%d %H:%M')}"
      ]

      if issue[:labels]&.any?
        labels = issue[:labels].map { |l| l[:name] }.join(", ")
        issue_info << "   Labels: #{labels}"
      end

      render(text: issue_info.join("\n"))
      render(text: "\n")
    end

    render(text: "📊 Summary: #{issues.count} issues retrieved (state: #{state_param})")
  rescue Octokit::Error => e
    report_error("Failed to fetch issues: #{e.message}")
  rescue => e
    report_error("Error fetching issues: #{e.message}")
  end
end
