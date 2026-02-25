# frozen_string_literal: true

class GithubSearchReposTool < ApplicationMCPTool
  tool_name "github_search_repos"
  description "Search for GitHub repositories using a query. Supports GitHub search syntax (e.g., 'language:ruby stars:>100'). Returns repository name, description, stars, language, and URL."

  property :query, type: "string", description: "Search query (supports GitHub search syntax like 'language:ruby stars:>100')", required: true
  property :sort, type: "string", description: "Sort by: stars, forks, help-wanted-issues, updated (optional)", required: false
  property :per_page, type: "integer", description: "Number of results per page (default: 30, max: 100)", required: false

  validates :query, presence: true, length: { minimum: 1 }

  def perform
    # Search is global — any installation token provides authenticated rate limits.
    # Try repo hint from query first, fall back to first available installation.
    hint = query_repo_hint
    installation_id = hint ? ::Github::App.installation_id_for_repo(hint) : nil
    installation_id ||= ::Github::App.first_installation_id
    unless installation_id
      report_error("GitHub App has no installations available for search")
      return
    end

    client = ::Github::App.installation_client(installation_id)

    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Searching repositories for: '#{query}' (via #{client_info})...")

    # Set options
    options = {}
    options[:sort] = sort if sort.present?
    options[:per_page] = (per_page || 30).to_i.clamp(1, 100)

    result = client.search_repositories(query, options)
    repos = result.items

    if repos.empty?
      render(text: "No repositories found matching query: '#{query}'")
      return
    end

    render(text: "Found #{result.total_count} repositories (showing #{repos.count}):\n")

    repos.each_with_index do |repo_item, index|
      updated_at = repo_item[:updated_at].is_a?(String) ? Time.parse(repo_item[:updated_at]) : repo_item[:updated_at]

      repo_info = [
        "#{index + 1}. 📦 #{repo_item[:full_name]}",
        "   Description: #{repo_item[:description] || 'No description'}",
        "   ⭐ Stars: #{repo_item[:stargazers_count]} | 🍴 Forks: #{repo_item[:forks_count]}",
        "   Language: #{repo_item[:language] || 'None'}",
        "   URL: #{repo_item[:html_url]}",
        "   Updated: #{updated_at.strftime('%Y-%m-%d')}"
      ]

      if repo_item[:topics]&.any?
        repo_info << "   Topics: #{repo_item[:topics].join(', ')}"
      end

      render(text: repo_info.join("\n"))
      render(text: "\n")
    end

    summary = [
      "📊 Search Summary:",
      "   Query: #{query}",
      "   Total Results: #{result.total_count}",
      "   Showing: #{repos.count}",
      sort.present? ? "   Sorted by: #{sort}" : nil
    ].compact

    render(text: summary.join("\n"))
  rescue Octokit::Error => e
    report_error("Failed to search repositories: #{e.message}")
  rescue => e
    report_error("Error searching repositories: #{e.message}")
  end

  private

  # Try to extract an owner/repo from the query for installation lookup.
  # Falls back to using the first available installation.
  def query_repo_hint
    # If query contains owner/repo pattern, use it
    match = query.match(/\b([\w\-\.]+\/[\w\-\.]+)\b/)
    match ? match[1] : nil
  end
end
