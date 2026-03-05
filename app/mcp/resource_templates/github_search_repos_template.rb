# frozen_string_literal: true

class GithubSearchReposTemplate < ApplicationMCPResTemplate
  uri_template "github://search/repos?q={query}"
  template_name "github_search_repos"
  description "Search for GitHub repositories using a query. Supports GitHub search syntax (e.g., 'language:ruby stars:>100'). Returns repository name, description, stars, language, and URL."
  mime_type "application/json"

  parameter :query, description: "Search query (supports GitHub search syntax like 'language:ruby stars:>100')", required: true
  parameter :sort, description: "Sort by: stars, forks, help-wanted-issues, updated (optional)", required: false
  parameter :per_page, description: "Number of results per page (default: 30, max: 100)", required: false

  def resolve
    # Search is global — any installation token provides authenticated rate limits.
    # Try repo hint from query first, fall back to first available installation.
    hint = query_repo_hint
    installation_id = hint ? ::Github::App.installation_id_for_repo(hint) : nil
    installation_id ||= ::Github::App.first_installation_id
    unless installation_id
      return { error: "GitHub App has no installations available for search" }.to_json
    end

    client = ::Github::App.installation_client(installation_id)

    options = {}
    options[:sort] = sort if sort.present?
    options[:per_page] = (per_page || 30).to_i.clamp(1, 100)

    result = client.search_repositories(query, options)
    repos = result.items

    {
      total_count: result.total_count,
      items: repos.map do |repo_item|
        {
          full_name: repo_item[:full_name],
          description: repo_item[:description],
          stargazers_count: repo_item[:stargazers_count],
          forks_count: repo_item[:forks_count],
          language: repo_item[:language],
          html_url: repo_item[:html_url],
          updated_at: repo_item[:updated_at],
          topics: repo_item[:topics]
        }.compact
      end
    }.to_json
  rescue Octokit::Error => e
    { error: "Failed to search repositories: #{e.message}" }.to_json
  rescue => e
    { error: "Error searching repositories: #{e.message}" }.to_json
  end

  private

  def query
    arguments["query"]
  end

  def sort
    arguments["sort"]
  end

  def per_page
    arguments["per_page"]
  end

  # Try to extract an owner/repo from the query for installation lookup.
  def query_repo_hint
    match = query.match(/\b([\w\-\.]+\/[\w\-\.]+)\b/)
    match ? match[1] : nil
  end
end
