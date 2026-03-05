# frozen_string_literal: true

class GithubIssueTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/issues/{number}"
  template_name "github_issue"
  description "Get detailed information about a specific GitHub issue or pull request. Requires the repository owner, repo name, and issue number."
  mime_type "application/json"

  parameter :owner, description: "Repository owner", required: true
  parameter :repo, description: "Repository name", required: true
  parameter :number, description: "Issue or PR number", required: true

  def resolve
    full_repo = "#{owner}/#{repo}"
    installation_id = ::Github::App.installation_id_for_repo(full_repo)
    raise "GitHub App is not installed on #{full_repo}" unless installation_id

    client = ::Github::App.installation_client(installation_id)
    issue = client.issue(full_repo, number.to_i)

    ActionMCP::Content::Resource.new(
      "github://repos/#{owner}/#{repo}/issues/#{number}",
      "application/json",
      text: {
        number: issue[:number],
        title: issue[:title],
        state: issue[:state],
        html_url: issue[:html_url],
        author: issue[:user][:login],
        comments: issue[:comments],
        created_at: issue[:created_at],
        updated_at: issue[:updated_at],
        closed_at: issue[:closed_at],
        assignees: issue[:assignees]&.map { |a| a[:login] },
        labels: issue[:labels]&.map { |l| l[:name] },
        milestone: issue[:milestone]&.dig(:title),
        body: issue[:body],
        is_pull_request: issue[:pull_request].present?,
        pull_request: issue[:pull_request].present? ? {
          merged_at: issue[:pull_request][:merged_at],
          diff_url: issue[:pull_request][:diff_url],
          patch_url: issue[:pull_request][:patch_url]
        } : nil
      }.compact.to_json
    )
  rescue Octokit::Error => e
    raise "Failed to fetch issue: #{e.message}"
  end
end
