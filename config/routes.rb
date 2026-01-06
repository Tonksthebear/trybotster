Rails.application.routes.draw do
  # Devise routes for session management (without OmniAuth callbacks)
  devise_for :users, skip: [ :sessions, :registrations, :omniauth_callbacks ]

  devise_scope :user do
    delete "logout", to: "users/sessions#destroy", as: :destroy_user_session
  end

  # GitHub namespace - all GitHub-related controllers
  namespace :github do
    # OAuth - RESTful resource for authorization
    resource :authorization, only: [ :new, :destroy ]

    # OAuth callback - separate controller for handling GitHub redirects
    resource :callback, only: [ :show ]  # GitHub OAuth callback (external constraint)

    # Webhooks - Exception: External API naming constraints
    post "webhooks", to: "webhooks#receive"  # GitHub webhook endpoint (external constraint)
  end

  # Botster Hub - RESTful resources
  namespace :bots do
    resources :messages, only: [ :index, :update ] # update for acknowledgment
  end

  # API namespace for CLI and browser communication
  namespace :api do
    resources :agent_notifications, only: [ :create ]

    # Device authorization flow (RFC 8628)
    resources :device_codes, only: [ :create, :show ], param: :device_code

    # Device registration for E2E encryption
    resources :devices, only: [ :index, :create, :destroy ] do
      resource :heartbeat, only: [ :update ], controller: "devices/heartbeats"
    end

    # Hub management (CLI registers/updates hubs)
    resources :hubs, param: :identifier, only: [ :index, :show, :update, :destroy ] do
      # Connection info for E2E key exchange (browser fetches CLI's public key)
      resource :connection, only: [ :show ], controller: "hubs/connections"
    end
  end

  # Device authorization (browser UI)
  get "device", to: "device#new", as: :device
  post "device", to: "device#lookup"
  get "device/confirm", to: "device#confirm", as: :device_confirm
  post "device/approve", to: "device#approve", as: :device_approve
  post "device/deny", to: "device#deny", as: :device_deny

  # Agents dashboard - WebRTC P2P connection to local CLI
  resources :agents, only: [ :index ] do
    collection do
      # Secure E2E connection - key exchange via URL fragment (MITM-proof)
      get :connect
    end
  end

  # Hubs dashboard - live view of active CLI instances
  resources :hubs, only: [ :index ]

  # Connect to a hub by entering connection code
  get "connect", to: "hubs#connect"
  post "connect", to: "hubs#lookup"

  # Tunnel sharing management (authenticated)
  resources :tunnel_shares, only: [ :create, :destroy ], param: :hub_agent_id

  # Private tunnel preview (authenticated, user's own hubs only)
  # format: false prevents Rails from extracting .css/.js as format (keeps full path)
  get "preview/:hub_id/:agent_id", to: "preview#proxy", as: :tunnel_root, defaults: { path: "" }
  get "preview/:hub_id/:agent_id/sw.js", to: "preview#service_worker", as: :tunnel_service_worker
  get "preview/:hub_id/:agent_id/*path", to: "preview#proxy", as: :tunnel_preview, format: false
  post "preview/:hub_id/:agent_id/*path", to: "preview#proxy", format: false
  patch "preview/:hub_id/:agent_id/*path", to: "preview#proxy", format: false
  put "preview/:hub_id/:agent_id/*path", to: "preview#proxy", format: false
  delete "preview/:hub_id/:agent_id/*path", to: "preview#proxy", format: false

  # Public shared tunnel (token-based, no auth required)
  get "share/:token", to: "shared_tunnels#proxy", as: :shared_tunnel, defaults: { path: "" }
  get "share/:token/*path", to: "shared_tunnels#proxy"
  post "share/:token/*path", to: "shared_tunnels#proxy"
  patch "share/:token/*path", to: "shared_tunnels#proxy"
  put "share/:token/*path", to: "shared_tunnels#proxy"
  delete "share/:token/*path", to: "shared_tunnels#proxy"

  root to: "home#index"
  # Define your application routes per the DSL in https://guides.rubyonrails.org/routing.html

  # Reveal health status on /up that returns 200 if the app boots with no exceptions, otherwise 500.
  # Can be used by load balancers and uptime monitors to verify that the app is live.
  get "up" => "rails/health#show", as: :rails_health_check

  # Render dynamic PWA files from app/views/pwa/* (remember to link manifest in application.html.erb)
  # get "manifest" => "rails/pwa#manifest", as: :pwa_manifest
  # get "service-worker" => "rails/pwa#service_worker", as: :pwa_service_worker
end
