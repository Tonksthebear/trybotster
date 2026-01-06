# frozen_string_literal: true

class DeviceController < ApplicationController
  before_action :authenticate_user!
  before_action :find_authorization, only: [ :confirm, :approve, :deny ]

  # GET /device
  # Form to enter device code
  def new
    # Just render the form
  end

  # POST /device
  # Look up the code and redirect to confirm
  def lookup
    code = normalize_user_code(params[:user_code])
    authorization = DeviceAuthorization.find_by(user_code: code) if code.present?

    if authorization.nil?
      redirect_to device_path, alert: "Invalid or expired code. Please check and try again."
      return
    end

    if authorization.expired?
      redirect_to device_path, alert: "This code has expired. Please request a new code from the CLI."
      return
    end

    unless authorization.pending?
      redirect_to device_path, alert: "This code has already been used."
      return
    end

    # Redirect to confirmation page
    redirect_to device_confirm_path(user_code: authorization.user_code)
  end

  # GET /device/confirm
  # Show confirmation page with approve/deny buttons
  def confirm
    if @authorization.nil? || @authorization.expired? || !@authorization.pending?
      redirect_to device_path, alert: "Invalid or expired code."
      return
    end

    # Render confirmation page
  end

  # POST /device/approve
  # Approve the device and issue token
  def approve
    if @authorization.nil? || @authorization.expired? || !@authorization.pending?
      redirect_to device_path, alert: "Invalid or expired code."
      return
    end

    @authorization.approve!(current_user)
    redirect_to agents_path, notice: "Device authorized! The CLI should now be connected."
  end

  # POST /device/deny
  # Deny the device request
  def deny
    if @authorization && @authorization.pending?
      @authorization.deny!
    end

    redirect_to root_path, notice: "Device request denied."
  end

  private

  def find_authorization
    code = normalize_user_code(params[:user_code])
    @authorization = DeviceAuthorization.find_by(user_code: code) if code.present?
  end

  def normalize_user_code(code)
    return nil if code.blank?
    # Remove any hyphens/spaces and uppercase
    code.gsub(/[-\s]/, "").upcase
  end
end
