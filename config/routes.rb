Rails.application.routes.draw do
  # Devise routes for session management (without OmniAuth callbacks)
  devise_for :users, skip: [ :sessions, :registrations, :omniauth_callbacks ]

  devise_scope :user do
    delete "logout", to: "users/sessions#destroy", as: :destroy_user_session
  end

  # GitHub namespace - all GitHub-related controllers
  namespace :github do
    resource :authorization, only: [ :new, :destroy ]
    resource :callback, only: [ :show ]
    post "webhooks", to: "webhooks#receive"
  end

  # User adding hubs to their account (browser authorization flow)
  namespace :users do
    resources :hubs, only: [ :new, :create ]
  end

  # Hubs - the central resource (uses Rails ID, not local identifier)
  # POST /hubs - CLI registration (returns Rails ID)
  # PUT /hubs/:id - CLI heartbeat (updates existing hub)
  resources :hubs, only: [ :index, :show, :create, :update, :destroy ] do
    collection do
      scope module: :hubs do
        resources :codes, only: [ :create, :show ]
      end
    end

    scope module: :hubs do
      resource :heartbeat, only: [ :update ]
      resources :messages, only: [ :update ]
      resources :notifications, only: [ :create ]
      resource :connection, only: [ :show ]
      # Agent terminal view by index
      # /hubs/:hub_id/agents/:index - agent overview (redirects to PTY 0)
      # /hubs/:hub_id/agents/:index/ptys/:pty_index - specific PTY terminal
      resources :agents, only: [ :show ], param: :index do
        resources :ptys, only: [ :show ], param: :index, controller: "agents/ptys"

        # Preview - for PTYs with port forwarding
        # Shell page at /preview/shell (under SW scope, so controlled)
        # SW.js at /preview/sw.js
        # Proxied content at /preview/* (except shell and sw.js)
        get ":pty_index/preview/sw.js", to: "agents/previews#service_worker", as: :pty_service_worker
        get ":pty_index/preview/shell", to: "agents/previews#shell", as: :pty_preview_shell
        get ":pty_index/preview", to: "agents/previews#bootstrap", as: :pty_preview
      end
    end
  end

  # E2E devices (browser keypairs - no heartbeat, they're session-based)
  resources :devices, only: [ :index, :create, :destroy ]

  # User settings
  resource :settings, only: [ :show, :update ]

  root to: "home#index"

  # Health check for load balancers and uptime monitors
  get "up" => "rails/health#show", as: :rails_health_check

  # Test-only routes (system tests need direct sign-in for OAuth apps)
  if Rails.env.test?
    namespace :test do
      get "sessions/new", to: "sessions#new"
      post "sessions", to: "sessions#create"
    end
  end
end
