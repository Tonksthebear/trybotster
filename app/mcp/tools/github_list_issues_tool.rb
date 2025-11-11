# frozen_string_literal: true

class GithubListIssuesTool < ApplicationMCPTool
  tool_name "github_list_issues"
  description "List GitHub issues for the authenticated user. Can filter by assigned, created, mentioned, subscribed, or all. Can filter by state: open, closed, or all."

  property :filter, type: "string", description: "Filter: assigned, created, mentioned, subscribed, all (default: assigned)", required: false
  property :state, type: "string", description: "State: open, closed, all (default: open)", required: false

  def perform
    # Check if user has authorized GitHub App
    unless current_user&.github_app_authorized?
      report_error("GitHub App not authorized. Please authorize at /github_app/authorize")
      return
    end

    # Get valid token (auto-refreshes if needed)
    token = current_user.valid_github_app_token
    unless token
      report_error("Failed to get valid GitHub token. Please re-authorize the GitHub App.")
      return
    end

    filter_param = filter || "assigned"
    state_param = state || "open"

    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Fetching #{filter_param} issues (#{state_param}) via #{client_info}...")

    # Fetch issues using Octokit
    result = GithubAppService.get_user_issues(token, filter: filter_param, state: state_param)

    if result[:success]
      issues = result[:issues]

      if issues.empty?
        render(text: "No issues found with filter '#{filter_param}' and state '#{state_param}'.")
        return
      end

      render(text: "Found #{issues.count} issues:\n")

      issues.each do |issue|
        # Determine if it's a PR or issue
        is_pr = issue['pull_request'].present?
        icon = is_pr ? "🔀" : "🐛"

        issue_info = [
          "#{icon} ##{issue['number']}: #{issue['title']}",
          "   Repository: #{issue['repository_url']&.split('/')&.last(2)&.join('/')}",
          "   State: #{issue['state']} | Comments: #{issue['comments']}",
          "   URL: #{issue['html_url']}",
          "   Created: #{Time.parse(issue['created_at']).strftime('%Y-%m-%d %H:%M')}",
          "   Updated: #{Time.parse(issue['updated_at']).strftime('%Y-%m-%d %H:%M')}"
        ]

        if issue['labels']&.any?
          labels = issue['labels'].map { |l| l['name'] }.join(', ')
          issue_info << "   Labels: #{labels}"
        end

        render(text: issue_info.join("\n"))
        render(text: "\n")
      end

      render(text: "📊 Summary: #{issues.count} issues retrieved (filter: #{filter_param}, state: #{state_param})")
    else
      report_error("Failed to fetch issues: #{result[:error]}")
    end
  rescue => e
    report_error("Error fetching issues: #{e.message}")
  end
end
