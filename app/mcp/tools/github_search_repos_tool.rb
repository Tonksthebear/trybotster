# frozen_string_literal: true

class GithubSearchReposTool < ApplicationMCPTool
  tool_name "github_search_repos"
  description "Search for GitHub repositories using a query. Supports GitHub search syntax (e.g., 'language:ruby stars:>100'). Returns repository name, description, stars, language, and URL."

  property :query, type: "string", description: "Search query (supports GitHub search syntax like 'language:ruby stars:>100')", required: true
  property :sort, type: "string", description: "Sort by: stars, forks, help-wanted-issues, updated (optional)", required: false
  property :per_page, type: "integer", description: "Number of results per page (default: 30, max: 100)", required: false

  validates :query, presence: true, length: { minimum: 1 }

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

    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Searching repositories for: '#{query}' (via #{client_info})...")

    # Set options
    options = {}
    options[:sort] = sort if sort.present?
    options[:per_page] = (per_page || 30).to_i.clamp(1, 100)

    # Search repositories using Octokit
    result = Github::App.search_repos(token, query: query, **options)

    if result[:success]
      repos = result[:repos]
      total_count = result[:total_count]

      if repos.empty?
        render(text: "No repositories found matching query: '#{query}'")
        return
      end

      render(text: "Found #{total_count} repositories (showing #{repos.count}):\n")

      repos.each_with_index do |repo, index|
        updated_at = repo['updated_at'].is_a?(String) ? Time.parse(repo['updated_at']) : repo['updated_at']

        repo_info = [
          "#{index + 1}. 📦 #{repo['full_name']}",
          "   Description: #{repo['description'] || 'No description'}",
          "   ⭐ Stars: #{repo['stargazers_count']} | 🍴 Forks: #{repo['forks_count']}",
          "   Language: #{repo['language'] || 'None'}",
          "   URL: #{repo['html_url']}",
          "   Updated: #{updated_at.strftime('%Y-%m-%d')}"
        ]

        if repo['topics']&.any?
          repo_info << "   Topics: #{repo['topics'].join(', ')}"
        end

        render(text: repo_info.join("\n"))
        render(text: "\n")
      end

      summary = [
        "📊 Search Summary:",
        "   Query: #{query}",
        "   Total Results: #{total_count}",
        "   Showing: #{repos.count}",
        sort.present? ? "   Sorted by: #{sort}" : nil
      ].compact

      render(text: summary.join("\n"))
    else
      report_error("Failed to search repositories: #{result[:error]}")
    end
  rescue => e
    report_error("Error searching repositories: #{e.message}")
  end
end
