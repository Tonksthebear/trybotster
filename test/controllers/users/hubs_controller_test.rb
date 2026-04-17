# frozen_string_literal: true

require "test_helper"

module Users
  class HubsControllerTest < ActionDispatch::IntegrationTest
    include Devise::Test::IntegrationHelpers

    setup do
      @user = users(:primary_user)
      sign_in @user
    end

    test "new renders the code entry form" do
      get new_users_hub_path

      assert_response :success
      assert_select "h1", text: "Connect Hub"
      assert_select "button", text: "Continue"
    end

    test "new with a valid code renders the approval form" do
      authorization = HubAuthorization.create!(device_name: "Test CLI")

      get new_users_hub_path(code: authorization.user_code)

      assert_response :success
      assert_select "h1", text: "Authorize Hub?"
      assert_select "button", text: "Approve"
    ensure
      authorization&.destroy
    end
  end
end
