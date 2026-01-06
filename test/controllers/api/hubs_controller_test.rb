# frozen_string_literal: true

require "test_helper"

module Api
  class HubsControllerTest < ActionDispatch::IntegrationTest
    setup do
      @user = User.create!(
        email: "hubs_test_user@example.com",
        username: "hubs_test_user"
      )
      @user.generate_api_key
      @user.save!

      @other_user = User.create!(
        email: "hubs_other_user@example.com",
        username: "hubs_other_user"
      )
      @other_user.generate_api_key
      @other_user.save!
    end

    teardown do
      @user&.destroy
      @other_user&.destroy
    end

    test "update requires API key" do
      put api_hub_url(identifier: "test-hub-123"),
          params: { repo: "owner/repo", agents: [] },
          as: :json

      assert_response :unauthorized
      json = JSON.parse(response.body)
      assert_equal "API key required", json["error"]
    end

    test "update with invalid API key returns unauthorized" do
      put api_hub_url(identifier: "test-hub-123"),
          params: { repo: "owner/repo", agents: [] },
          headers: { "X-API-Key" => "invalid_key" },
          as: :json

      assert_response :unauthorized
      json = JSON.parse(response.body)
      assert_equal "Invalid API key", json["error"]
    end

    test "update creates new hub" do
      identifier = SecureRandom.uuid

      assert_difference("Hub.count", 1) do
        put api_hub_url(identifier: identifier),
            params: { repo: "owner/repo", agents: [] },
            headers: { "X-API-Key" => @user.api_key },
            as: :json
      end

      assert_response :success
      json = JSON.parse(response.body)
      assert json["success"]
      assert json["hub_id"]

      hub = Hub.find(json["hub_id"])
      assert_equal "owner/repo", hub.repo
      assert_equal identifier, hub.identifier
      assert_equal @user, hub.user
    end

    test "update updates existing hub" do
      identifier = SecureRandom.uuid
      hub = Hub.create!(
        user: @user,
        repo: "owner/old-repo",
        identifier: identifier,
        last_seen_at: 10.minutes.ago
      )

      original_last_seen = hub.last_seen_at

      put api_hub_url(identifier: identifier),
          params: { repo: "owner/new-repo", agents: [] },
          headers: { "X-API-Key" => @user.api_key },
          as: :json

      assert_response :success
      hub.reload
      assert_equal "owner/new-repo", hub.repo
      assert hub.last_seen_at > original_last_seen
    end

    test "update syncs agents" do
      identifier = SecureRandom.uuid
      hub = Hub.create!(
        user: @user,
        repo: "owner/repo",
        identifier: identifier,
        last_seen_at: Time.current
      )

      agents_data = [
        { session_key: "owner-repo-1", last_invocation_url: "https://github.com/owner/repo/issues/1" },
        { session_key: "owner-repo-2", last_invocation_url: "https://github.com/owner/repo/pull/2" }
      ]

      assert_difference("HubAgent.count", 2) do
        put api_hub_url(identifier: identifier),
            params: { repo: "owner/repo", agents: agents_data },
            headers: { "X-API-Key" => @user.api_key },
            as: :json
      end

      assert_response :success
      hub.reload
      assert_equal 2, hub.hub_agents.count
      assert_equal "owner-repo-1", hub.hub_agents.find_by(session_key: "owner-repo-1").session_key
    end

    test "update removes stale agents" do
      identifier = SecureRandom.uuid
      hub = Hub.create!(
        user: @user,
        repo: "owner/repo",
        identifier: identifier,
        last_seen_at: Time.current
      )
      agent1 = hub.hub_agents.create!(session_key: "owner-repo-1")
      agent2 = hub.hub_agents.create!(session_key: "owner-repo-2")

      # Only send agent1 in the heartbeat
      put api_hub_url(identifier: identifier),
          params: { repo: "owner/repo", agents: [ { session_key: "owner-repo-1" } ] },
          headers: { "X-API-Key" => @user.api_key },
          as: :json

      assert_response :success
      hub.reload
      assert_equal 1, hub.hub_agents.count
      assert_equal "owner-repo-1", hub.hub_agents.first.session_key
      assert_raises(ActiveRecord::RecordNotFound) { agent2.reload }
    end

    test "update cannot access other user's hub" do
      identifier = SecureRandom.uuid
      # Create hub owned by @other_user
      Hub.create!(
        user: @other_user,
        repo: "other/repo",
        identifier: identifier,
        last_seen_at: Time.current
      )

      # Try to update with @user's API key - should create a new hub for @user
      put api_hub_url(identifier: identifier),
          params: { repo: "owner/repo", agents: [] },
          headers: { "X-API-Key" => @user.api_key },
          as: :json

      # Should fail because identifier is unique across all users
      assert_response :unprocessable_entity
    end

    test "destroy requires API key" do
      delete api_hub_url(identifier: "test-hub-123"),
             as: :json

      assert_response :unauthorized
    end

    test "destroy deletes hub" do
      identifier = SecureRandom.uuid
      hub = Hub.create!(
        user: @user,
        repo: "owner/repo",
        identifier: identifier,
        last_seen_at: Time.current
      )

      assert_difference("Hub.count", -1) do
        delete api_hub_url(identifier: identifier),
               headers: { "X-API-Key" => @user.api_key },
               as: :json
      end

      assert_response :success
      json = JSON.parse(response.body)
      assert json["success"]
    end

    test "destroy is idempotent for non-existent hub" do
      delete api_hub_url(identifier: "non-existent-hub"),
             headers: { "X-API-Key" => @user.api_key },
             as: :json

      assert_response :success
      json = JSON.parse(response.body)
      assert json["success"]
    end

    test "destroy cannot delete other user's hub" do
      identifier = SecureRandom.uuid
      hub = Hub.create!(
        user: @other_user,
        repo: "other/repo",
        identifier: identifier,
        last_seen_at: Time.current
      )

      assert_no_difference("Hub.count") do
        delete api_hub_url(identifier: identifier),
               headers: { "X-API-Key" => @user.api_key },
               as: :json
      end

      assert_response :success # Idempotent - returns success even if not found for this user
      assert hub.reload # Hub still exists
    end

    test "destroy cascades to hub_agents" do
      identifier = SecureRandom.uuid
      hub = Hub.create!(
        user: @user,
        repo: "owner/repo",
        identifier: identifier,
        last_seen_at: Time.current
      )
      hub.hub_agents.create!(session_key: "owner-repo-1")
      hub.hub_agents.create!(session_key: "owner-repo-2")

      assert_difference("HubAgent.count", -2) do
        delete api_hub_url(identifier: identifier),
               headers: { "X-API-Key" => @user.api_key },
               as: :json
      end

      assert_response :success
    end

    # Device association tests - critical for E2E encryption
    test "update with device_id associates device with hub" do
      device = Device.create!(
        user: @user,
        public_key: "test_public_key_base64",
        device_type: "cli",
        name: "Test CLI Device"
      )
      identifier = SecureRandom.uuid

      put api_hub_url(identifier: identifier),
          params: { repo: "owner/repo", agents: [], device_id: device.id },
          headers: { "X-API-Key" => @user.api_key },
          as: :json

      assert_response :success
      json = JSON.parse(response.body)
      assert json["e2e_enabled"], "E2E should be enabled when device is associated"

      hub = Hub.find(json["hub_id"])
      assert_equal device, hub.device, "Hub should be associated with the device"
    end

    test "update without device_id leaves hub device nil" do
      identifier = SecureRandom.uuid

      put api_hub_url(identifier: identifier),
          params: { repo: "owner/repo", agents: [] },
          headers: { "X-API-Key" => @user.api_key },
          as: :json

      assert_response :success
      json = JSON.parse(response.body)
      refute json["e2e_enabled"], "E2E should be disabled when no device is associated"

      hub = Hub.find(json["hub_id"])
      assert_nil hub.device, "Hub should not have a device when device_id not provided"
    end

    test "update with invalid device_id ignores device association" do
      identifier = SecureRandom.uuid

      put api_hub_url(identifier: identifier),
          params: { repo: "owner/repo", agents: [], device_id: 999999 },
          headers: { "X-API-Key" => @user.api_key },
          as: :json

      assert_response :success
      json = JSON.parse(response.body)
      refute json["e2e_enabled"], "E2E should be disabled when device_id is invalid"

      hub = Hub.find(json["hub_id"])
      assert_nil hub.device, "Hub should not have a device when device_id is invalid"
    end

    test "update with other user device_id ignores device association" do
      other_device = Device.create!(
        user: @other_user,
        public_key: "other_user_public_key",
        device_type: "cli",
        name: "Other User CLI"
      )
      identifier = SecureRandom.uuid

      put api_hub_url(identifier: identifier),
          params: { repo: "owner/repo", agents: [], device_id: other_device.id },
          headers: { "X-API-Key" => @user.api_key },
          as: :json

      assert_response :success
      json = JSON.parse(response.body)
      refute json["e2e_enabled"], "E2E should be disabled when device belongs to other user"

      hub = Hub.find(json["hub_id"])
      assert_nil hub.device, "Hub should not have a device that belongs to another user"
    end
  end
end
