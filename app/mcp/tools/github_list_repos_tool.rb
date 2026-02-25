# frozen_string_literal: true

class GithubListReposTool < ApplicationMCPTool
  tool_name "github_list_repos"
  description "List GitHub repositories accessible to the bot. Returns repository name, description, URL, language, stars, and last update."

  property :per_page, type: "integer", description: "Number of repositories per page (default: 30, max: 100)", required: false
  property :sort, type: "string", description: "Sort by: created, updated, pushed, full_name (default: updated)", required: false
  property :direction, type: "string", description: "Sort direction: asc or desc (default: desc)", required: false

  def perform
    # Detect client for user feedback
    client_info = detect_client_type
    render(text: "Fetching accessible GitHub repositories (via #{client_info})...")

    repos = ::Github::App.list_installation_repos

    if repos.empty?
      render(text: "No repositories found. Ensure the GitHub App is installed on at least one repository.")
      return
    end

    # Apply sorting
    sort_field = sort || "updated_at"
    sort_dir = direction || "desc"
    repos.sort_by! { |r| r[sort_field.to_sym] || r[sort_field] || "" }
    repos.reverse! if sort_dir == "desc"

    # Apply pagination
    limit = (per_page || 30).to_i.clamp(1, 100)
    repos = repos.first(limit)

    render(text: "Found #{repos.count} repositories:\n")

    repos.each do |repo|
      updated_at = repo[:updated_at].is_a?(String) ? Time.parse(repo[:updated_at]) : repo[:updated_at]

      repo_info = [
        "📦 #{repo[:full_name]}",
        "   Description: #{repo[:description] || 'No description'}",
        "   URL: #{repo[:html_url]}",
        "   Language: #{repo[:language] || 'None'}",
        "   ⭐ Stars: #{repo[:stargazers_count]}",
        "   🔄 Updated: #{updated_at&.strftime('%Y-%m-%d %H:%M') || 'Unknown'}"
      ]

      render(text: repo_info.join("\n"))
      render(text: "\n")
    end

    render(text: "\n📊 Summary: #{repos.count} repositories retrieved")
  rescue => e
    report_error("Error fetching repositories: #{e.message}")
  end
end
