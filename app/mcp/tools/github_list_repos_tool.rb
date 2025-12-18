# frozen_string_literal: true

class GithubListReposTool < ApplicationMCPTool
  tool_name "github_list_repos"
  description "List GitHub repositories for the authenticated user. Returns repository name, description, URL, language, stars, and last update."

  property :per_page, type: "integer", description: "Number of repositories per page (default: 30, max: 100)", required: false
  property :sort, type: "string", description: "Sort by: created, updated, pushed, full_name (default: updated)", required: false
  property :direction, type: "string", description: "Sort direction: asc or desc (default: desc)", required: false

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
    render(text: "Fetching your GitHub repositories (via #{client_info})...")

    # Set defaults
    options = {
      per_page: (per_page || 30).to_i.clamp(1, 100)
    }
    options[:sort] = sort if sort.present?
    options[:direction] = direction if direction.present?

    # Fetch repositories using Octokit
    result = Github::App.get_user_repos(token, **options)

    if result[:success]
      repos = result[:repos]

      if repos.empty?
        render(text: "No repositories found.")
        return
      end

      render(text: "Found #{repos.count} repositories:\n")

      repos.each do |repo|
        updated_at = repo['updated_at'].is_a?(String) ? Time.parse(repo['updated_at']) : repo['updated_at']

        repo_info = [
          "📦 #{repo['full_name']}",
          "   Description: #{repo['description'] || 'No description'}",
          "   URL: #{repo['html_url']}",
          "   Language: #{repo['language'] || 'None'}",
          "   ⭐ Stars: #{repo['stargazers_count']}",
          "   🔄 Updated: #{updated_at.strftime('%Y-%m-%d %H:%M')}"
        ]

        render(text: repo_info.join("\n"))
        render(text: "\n")
      end

      # Return structured data as well
      summary = {
        total: repos.count,
        repositories: repos.map { |r|
          {
            name: r['name'],
            full_name: r['full_name'],
            description: r['description'],
            url: r['html_url'],
            language: r['language'],
            stars: r['stargazers_count'],
            forks: r['forks_count'],
            updated_at: r['updated_at'],
            private: r['private']
          }
        }
      }

      render(text: "\n📊 Summary: #{repos.count} repositories retrieved")
    else
      report_error("Failed to fetch repositories: #{result[:error]}")
    end
  rescue => e
    report_error("Error fetching repositories: #{e.message}")
  end
end
