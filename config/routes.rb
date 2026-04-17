Rails.application.routes.draw do
  # ActionCable WebSocket endpoint
  mount ActionCable.server => "/cable"

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
      resources :notifications, only: [ :create ]
      resource :webrtc, only: [ :show ], controller: :webrtc  # GET config
      resource :settings, only: [ :show, :update, :destroy ], controller: :settings
      resource :pairing, only: [ :show ], controller: :pairing
      # Session terminal view by session UUID
      # /hubs/:hub_id/sessions/:session_uuid - terminal for a specific session
      resources :sessions, only: [ :show ], param: :uuid do
      end
    end
  end

  # Integration-specific endpoints
  namespace :integrations do
    namespace :github do
      # MCP token creation/refresh for plugins (bearer auth via hub token)
      resources :mcp_tokens, only: [ :create ]
    end
  end

  # Browser key registration (E2E keypairs - backward compat with CLI API calls)
  resources :devices, only: [ :index, :create, :destroy ]

  # User settings
  resource :settings, only: [ :show ]

  # Documentation (public)
  get "docs", to: "docs#show", as: :docs
  get "docs/*path", to: "docs#show", as: :doc

  # SPA frontend routes — React Router handles navigation
  root to: "spa#home"

  # SPA catch-all for frontend paths that React Router manages.
  # Must be after all API/auth routes to avoid intercepting them.
  get "/hubs/:hub_id/sessions/*path", to: "spa#hub", as: nil
  get "/hubs/:hub_id/settings", to: "spa#hub", as: nil
  get "/hubs/:hub_id/pairing", to: "spa#hub", as: nil

  # PWA
  get "manifest" => "rails/pwa#manifest", as: :pwa_manifest
  get "service-worker" => "rails/pwa#service_worker", as: :pwa_service_worker

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
