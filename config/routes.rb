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

  # WebRTC signaling for P2P browser-to-CLI connections
  namespace :api do
    resources :webrtc_sessions, path: "webrtc/sessions", only: [ :create, :show, :update ]
  end

  # Agents dashboard - WebRTC P2P connection to local CLI
  resources :agents, only: [ :index ]

  root to: "home#index"
  # Define your application routes per the DSL in https://guides.rubyonrails.org/routing.html

  # Reveal health status on /up that returns 200 if the app boots with no exceptions, otherwise 500.
  # Can be used by load balancers and uptime monitors to verify that the app is live.
  get "up" => "rails/health#show", as: :rails_health_check

  # Render dynamic PWA files from app/views/pwa/* (remember to link manifest in application.html.erb)
  # get "manifest" => "rails/pwa#manifest", as: :pwa_manifest
  # get "service-worker" => "rails/pwa#service_worker", as: :pwa_service_worker
end
