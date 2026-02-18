# frozen_string_literal: true

module Users
  # Handles browser-side authorization flow: user "adds a hub to their account"
  # by entering and approving a device code from the CLI.
  class HubsController < ApplicationController
    layout "application"

    before_action :authenticate_user!

    # GET /users/hubs/new
    # Either show enter code form, or show confirmation if code provided
    def new
      if params[:code].present?
        @authorization = find_authorization(params[:code])

        if @authorization.nil? || @authorization.expired? || !@authorization.pending?
          redirect_to new_users_hub_path, alert: "Invalid or expired code."
          return
        end

        render :confirm
      end
      # else render :new (enter code form)
    end

    # POST /users/hubs
    # Two purposes:
    # 1. From enter code form: lookup code and redirect to confirmation
    # 2. From confirmation page: approve the authorization
    def create
      if params[:approve].present?
        approve_authorization
      else
        lookup_and_redirect
      end
    end

    private

    def approve_authorization
      @authorization = find_authorization(params[:code])

      if @authorization.nil? || @authorization.expired? || !@authorization.pending?
        redirect_to new_users_hub_path, alert: "Invalid or expired code."
        return
      end

      @authorization.approve!(current_user)
      redirect_to hubs_path, flash: { hub_booting: true }
    end

    def lookup_and_redirect
      code = normalize_user_code(params[:user_code])
      @authorization = find_authorization(code)

      if @authorization.nil?
        redirect_to new_users_hub_path, alert: "Invalid or expired code. Please check and try again."
        return
      end

      if @authorization.expired?
        redirect_to new_users_hub_path, alert: "This code has expired. Please request a new code from the CLI."
        return
      end

      unless @authorization.pending?
        redirect_to new_users_hub_path, alert: "This code has already been used."
        return
      end

      redirect_to new_users_hub_path(code: @authorization.user_code)
    end

    def find_authorization(code)
      normalized = normalize_user_code(code)
      DeviceAuthorization.find_by(user_code: normalized) if normalized.present?
    end

    def normalize_user_code(code)
      return nil if code.blank?
      code.gsub(/[-\s]/, "").upcase
    end
  end
end
