# frozen_string_literal: true

class GithubPullRequestTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/pulls/{number}"
  template_name "github_pull_request"
  description "Get detailed information about a specific GitHub pull request, including diff and merge status."
  mime_type "application/json"

  parameter :owner, description: "Repository owner", required: true
  parameter :repo, description: "Repository name", required: true
  parameter :number, description: "Pull request number", required: true

  def resolve
    full_repo = "#{owner}/#{repo}"
    installation_id = ::Github::App.installation_id_for_repo(full_repo)
    raise "GitHub App is not installed on #{full_repo}" unless installation_id

    client = ::Github::App.installation_client(installation_id)
    pr = client.pull_request(full_repo, number.to_i)

    ActionMCP::Content::Resource.new(
      "github://repos/#{owner}/#{repo}/pulls/#{number}",
      "application/json",
      text: {
        number: pr[:number],
        title: pr[:title],
        state: pr[:state],
        merged: pr[:merged],
        draft: pr[:draft],
        html_url: pr[:html_url],
        author: pr[:user][:login],
        comments: pr[:comments],
        commits: pr[:commits],
        additions: pr[:additions],
        deletions: pr[:deletions],
        changed_files: pr[:changed_files],
        created_at: pr[:created_at],
        updated_at: pr[:updated_at],
        merged_at: pr[:merged_at],
        closed_at: pr[:closed_at],
        merged_by: pr[:merged_by]&.dig(:login),
        assignees: pr[:assignees]&.map { |a| a[:login] },
        requested_reviewers: pr[:requested_reviewers]&.map { |r| r[:login] },
        labels: pr[:labels]&.map { |l| l[:name] },
        head: {
          ref: pr[:head][:ref],
          repo: pr[:head][:repo]&.dig(:full_name)
        },
        base: {
          ref: pr[:base][:ref],
          repo: pr[:base][:repo][:full_name]
        },
        mergeable: pr[:mergeable],
        mergeable_state: pr[:mergeable_state],
        body: pr[:body],
        diff_url: pr[:diff_url],
        patch_url: pr[:patch_url]
      }.compact.to_json
    )
  rescue Octokit::Error => e
    raise "Failed to fetch pull request: #{e.message}"
  end
end
