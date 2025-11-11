Rails.application.routes.draw do
  # Devise routes for session management (without OmniAuth callbacks)
  devise_for :users, skip: [:sessions, :registrations, :omniauth_callbacks]

  devise_scope :user do
    delete 'logout', to: 'users/sessions#destroy', as: :destroy_user_session
  end

  # GitHub App OAuth routes
  get 'github_app/authorize', to: 'github_app#authorize', as: :github_app_authorize
  get 'auth/github_app/callback', to: 'github_app#callback', as: :github_app_callback
  delete 'github_app/revoke', to: 'github_app#revoke', as: :github_app_revoke
  get 'github_app/status', to: 'github_app#status', as: :github_app_status

  # GitHub API example routes (demonstrating API usage)
  namespace :github do
    get 'repos', to: 'github_example#repos'
    get 'issues', to: 'github_example#issues'
    get 'pull_requests', to: 'github_example#pull_requests'
    post 'comment', to: 'github_example#create_comment'
    get 'status', to: 'github_example#status'
  end

  root to: "home#index"
  # Define your application routes per the DSL in https://guides.rubyonrails.org/routing.html

  # Reveal health status on /up that returns 200 if the app boots with no exceptions, otherwise 500.
  # Can be used by load balancers and uptime monitors to verify that the app is live.
  get "up" => "rails/health#show", as: :rails_health_check

  # Render dynamic PWA files from app/views/pwa/* (remember to link manifest in application.html.erb)
  # get "manifest" => "rails/pwa#manifest", as: :pwa_manifest
  # get "service-worker" => "rails/pwa#service_worker", as: :pwa_service_worker
end
