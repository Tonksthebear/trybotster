# frozen_string_literal: true

# Example controller demonstrating GitHub App API usage
# This shows how to interact with users' repos, issues, and PRs
class GithubExampleController < ApplicationController
  before_action :authenticate_user!
  before_action :ensure_github_app_authorized, except: [:index]

  # GET /github/repos
  # Show user's repositories
  def repos
    token = current_user.valid_github_app_token
    result = GithubAppService.get_user_repos(token, per_page: 20)

    if result[:success]
      @repos = result[:repos]
      render json: {
        success: true,
        count: @repos.size,
        repos: @repos.map { |repo|
          {
            name: repo['name'],
            full_name: repo['full_name'],
            description: repo['description'],
            url: repo['html_url'],
            language: repo['language'],
            stars: repo['stargazers_count'],
            updated_at: repo['updated_at']
          }
        }
      }
    else
      render json: { success: false, error: result[:error] }, status: :bad_request
    end
  end

  # GET /github/issues
  # Show user's issues
  def issues
    token = current_user.valid_github_app_token
    filter = params[:filter] || 'assigned' # assigned, created, mentioned, subscribed, all
    state = params[:state] || 'open' # open, closed, all

    result = GithubAppService.get_user_issues(token, filter: filter, state: state)

    if result[:success]
      @issues = result[:issues]
      render json: {
        success: true,
        count: @issues.size,
        issues: @issues.map { |issue|
          {
            number: issue['number'],
            title: issue['title'],
            state: issue['state'],
            url: issue['html_url'],
            repository: issue['repository_url']&.split('/')&.last(2)&.join('/'),
            created_at: issue['created_at'],
            updated_at: issue['updated_at']
          }
        }
      }
    else
      render json: { success: false, error: result[:error] }, status: :bad_request
    end
  end

  # GET /github/pull_requests
  # Show user's pull requests
  def pull_requests
    token = current_user.valid_github_app_token
    state = params[:state] || 'open' # open, closed, all

    result = GithubAppService.get_user_pull_requests(token, state: state)

    if result[:success]
      @prs = result[:pull_requests]
      render json: {
        success: true,
        count: @prs.size,
        pull_requests: @prs.map { |pr|
          {
            number: pr['number'],
            title: pr['title'],
            state: pr['state'],
            url: pr['html_url'],
            repository: pr['repository_url']&.split('/')&.last(2)&.join('/'),
            created_at: pr['created_at'],
            updated_at: pr['updated_at']
          }
        }
      }
    else
      render json: { success: false, error: result[:error] }, status: :bad_request
    end
  end

  # POST /github/comment
  # Create a comment on an issue or PR
  # Params: owner, repo, issue_number, body
  def create_comment
    token = current_user.valid_github_app_token

    owner = params[:owner]
    repo = params[:repo]
    issue_number = params[:issue_number]
    body = params[:body]

    if owner.blank? || repo.blank? || issue_number.blank? || body.blank?
      render json: {
        success: false,
        error: "Missing required parameters: owner, repo, issue_number, body"
      }, status: :unprocessable_entity
      return
    end

    result = GithubAppService.create_issue_comment(
      token,
      owner: owner,
      repo: repo,
      issue_number: issue_number.to_i,
      body: body
    )

    if result[:success]
      render json: {
        success: true,
        comment: {
          id: result[:comment]['id'],
          url: result[:comment]['html_url'],
          body: result[:comment]['body'],
          created_at: result[:comment]['created_at']
        }
      }
    else
      render json: { success: false, error: result[:error] }, status: :bad_request
    end
  end

  # GET /github/status
  # Check authorization status and token info
  def status
    render json: {
      authorized: current_user.github_app_authorized?,
      expires_at: current_user.github_app_token_expires_at,
      expired: current_user.github_app_token_expired?,
      needs_refresh: current_user.github_app_token_needs_refresh?,
      permissions: current_user.github_app_permissions
    }
  end

  private

  def ensure_github_app_authorized
    unless current_user.github_app_authorized?
      render json: {
        success: false,
        error: "GitHub App not authorized. Please authorize at /github_app/authorize"
      }, status: :unauthorized
    end
  end
end
