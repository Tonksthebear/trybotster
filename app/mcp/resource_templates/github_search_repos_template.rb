# frozen_string_literal: true

class GithubSearchReposTemplate < ApplicationMCPResTemplate
  uri_template "github://search/repos?q={query}"
  template_name "github_search_repos"
  description "Search for GitHub repositories using a query. Supports GitHub search syntax."
  mime_type "application/json"

  parameter :query, description: "Search query (supports GitHub search syntax)", required: true

  def resolve
    installation_id = query_repo_hint
    installation_id = installation_id ? ::Github::App.installation_id_for_repo(installation_id) : nil
    installation_id ||= ::Github::App.first_installation_id
    raise "GitHub App has no installations available for search" unless installation_id

    client = ::Github::App.installation_client(installation_id)
    result = client.search_repositories(query)

    ActionMCP::Content::Resource.new(
      "github://search/repos?q=#{query}",
      "application/json",
      text: {
        total_count: result.total_count,
        items: result.items.map(&:to_h)
      }.to_json
    )
  rescue Octokit::Error => e
    raise "Failed to search repositories: #{e.message}"
  end

  private

  def query_repo_hint
    match = query.match(/\b([\w\-\.]+\/[\w\-\.]+)\b/)
    match ? match[1] : nil
  end
end
