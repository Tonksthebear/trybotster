# frozen_string_literal: true

require "test_helper"

class TunnelSharesControllerTest < ActionDispatch::IntegrationTest
  include Devise::Test::IntegrationHelpers

  setup do
    @user = users(:one)
    @other_user = users(:two)

    @hub = Hub.create!(
      user: @user,
      repo: "owner/repo",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )

    @hub_agent = HubAgent.create!(
      hub: @hub,
      session_key: "owner-repo-123",
      tunnel_port: 4001,
      tunnel_status: "connected"
    )

    @other_hub = Hub.create!(
      user: @other_user,
      repo: "other/repo",
      identifier: SecureRandom.uuid,
      last_seen_at: Time.current
    )

    @other_hub_agent = HubAgent.create!(
      hub: @other_hub,
      session_key: "other-repo-456",
      tunnel_port: 4002,
      tunnel_status: "connected"
    )
  end

  teardown do
    @hub&.destroy
    @other_hub&.destroy
  end

  # CREATE tests
  test "create requires authentication" do
    post tunnel_shares_url(hub_agent_id: @hub_agent.id), as: :json

    assert_response :unauthorized
  end

  test "create enables sharing and generates token" do
    sign_in @user

    assert_not @hub_agent.sharing_enabled?

    post tunnel_shares_url(hub_agent_id: @hub_agent.id), as: :json

    assert_response :success

    @hub_agent.reload
    assert @hub_agent.sharing_enabled?
    assert_not_nil @hub_agent.tunnel_share_token

    json = JSON.parse(response.body)
    assert_includes json["share_url"], @hub_agent.tunnel_share_token
  end

  test "create returns html redirect for html format" do
    sign_in @user

    post tunnel_shares_url(hub_agent_id: @hub_agent.id)

    assert_response :redirect
  end

  test "create cannot enable sharing for other user's agent" do
    sign_in @user

    post tunnel_shares_url(hub_agent_id: @other_hub_agent.id), as: :json

    assert_response :not_found

    @other_hub_agent.reload
    assert_not @other_hub_agent.sharing_enabled?
  end

  test "create returns not found for non-existent agent" do
    sign_in @user

    post tunnel_shares_url(hub_agent_id: 999999), as: :json

    assert_response :not_found
  end

  # DESTROY tests
  test "destroy requires authentication" do
    @hub_agent.enable_sharing!

    delete tunnel_share_url(hub_agent_id: @hub_agent.id), as: :json

    assert_response :unauthorized
  end

  test "destroy disables sharing and clears token" do
    sign_in @user
    @hub_agent.enable_sharing!

    assert @hub_agent.sharing_enabled?

    delete tunnel_share_url(hub_agent_id: @hub_agent.id), as: :json

    assert_response :success

    @hub_agent.reload
    assert_not @hub_agent.sharing_enabled?
    assert_nil @hub_agent.tunnel_share_token

    json = JSON.parse(response.body)
    assert json["success"]
  end

  test "destroy returns html redirect for html format" do
    sign_in @user
    @hub_agent.enable_sharing!

    delete tunnel_share_url(hub_agent_id: @hub_agent.id)

    assert_response :redirect
  end

  test "destroy cannot disable sharing for other user's agent" do
    sign_in @user
    @other_hub_agent.enable_sharing!
    original_token = @other_hub_agent.tunnel_share_token

    delete tunnel_share_url(hub_agent_id: @other_hub_agent.id), as: :json

    assert_response :not_found

    @other_hub_agent.reload
    assert @other_hub_agent.sharing_enabled?
    assert_equal original_token, @other_hub_agent.tunnel_share_token
  end

  test "destroy returns not found for non-existent agent" do
    sign_in @user

    delete tunnel_share_url(hub_agent_id: 999999), as: :json

    assert_response :not_found
  end

  test "destroy is idempotent for already disabled sharing" do
    sign_in @user

    assert_not @hub_agent.sharing_enabled?

    delete tunnel_share_url(hub_agent_id: @hub_agent.id), as: :json

    assert_response :success

    @hub_agent.reload
    assert_not @hub_agent.sharing_enabled?
  end

  # Token uniqueness
  test "each enable_sharing generates unique token" do
    sign_in @user

    post tunnel_shares_url(hub_agent_id: @hub_agent.id), as: :json
    @hub_agent.reload
    first_token = @hub_agent.tunnel_share_token

    delete tunnel_share_url(hub_agent_id: @hub_agent.id), as: :json

    post tunnel_shares_url(hub_agent_id: @hub_agent.id), as: :json
    @hub_agent.reload
    second_token = @hub_agent.tunnel_share_token

    assert_not_nil first_token
    assert_not_nil second_token
    assert_not_equal first_token, second_token
  end
end
