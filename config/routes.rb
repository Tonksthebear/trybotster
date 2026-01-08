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

  # Hubs - the central resource
  resources :hubs, param: :identifier, only: [ :index, :show, :update, :destroy ] do
    collection do
      scope module: :hubs do
        resources :codes, only: [ :create, :show ]
      end
    end

    scope module: :hubs do
      resource :heartbeat, only: [ :update ]
      resources :messages, only: [ :index, :update ]
      resources :notifications, only: [ :create ]
      resource :connection, only: [ :show ]
    end
  end

  # E2E devices (browser keypairs - no heartbeat, they're session-based)
  resources :devices, only: [ :index, :create, :destroy ]

  # User settings
  resource :settings, only: [ :show, :update ]

  # Private tunnel preview (authenticated, user's own hubs only)
  get "preview/:hub_id/:agent_id", to: "preview#proxy", as: :tunnel_root, defaults: { path: "" }
  get "preview/:hub_id/:agent_id/sw.js", to: "preview#service_worker", as: :tunnel_service_worker
  get "preview/:hub_id/:agent_id/*path", to: "preview#proxy", as: :tunnel_preview, format: false
  match "preview/:hub_id/:agent_id/*path", to: "preview#proxy", via: [ :post, :patch, :put, :delete ], format: false

  root to: "home#index"

  # Health check for load balancers and uptime monitors
  get "up" => "rails/health#show", as: :rails_health_check
end
