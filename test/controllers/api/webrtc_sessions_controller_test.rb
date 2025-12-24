# frozen_string_literal: true

require "test_helper"

module Api
  class WebrtcSessionsControllerTest < ActionDispatch::IntegrationTest
    include Devise::Test::IntegrationHelpers

    setup do
      @user = users(:one)
      @other_user = users(:two)
      @valid_offer = { type: "offer", sdp: "v=0\r\no=- 123 2 IN IP4 127.0.0.1\r\n..." }
      @valid_answer = { type: "answer", sdp: "v=0\r\no=- 456 2 IN IP4 127.0.0.1\r\n..." }
    end

    # Browser creates session (requires session auth)
    test "create requires authentication" do
      post api_webrtc_sessions_url, params: { offer: @valid_offer }, as: :json
      assert_response :unauthorized
    end

    test "create with valid offer creates session and bot message" do
      sign_in @user

      assert_difference [ "WebrtcSession.count", "Bot::Message.count" ], 1 do
        post api_webrtc_sessions_url, params: { offer: @valid_offer }, as: :json
      end

      assert_response :created
      json = JSON.parse(response.body)

      assert_not_nil json["session_id"]
      assert_equal "pending", json["status"]

      # Check the session was created correctly
      session = WebrtcSession.find(json["session_id"])
      assert_equal @user, session.user
      assert_equal @valid_offer.stringify_keys, session.offer

      # Check the bot message was created
      message = Bot::Message.last
      assert_equal "webrtc_offer", message.event_type
      assert_equal session.id.to_s, message.payload["session_id"]
    end

    test "create with missing offer returns error" do
      sign_in @user

      post api_webrtc_sessions_url, params: {}, as: :json

      assert_response :unprocessable_entity
    end

    # Browser polls for answer (requires session auth)
    test "show requires authentication" do
      session = WebrtcSession.create!(user: @user, offer: @valid_offer)

      get api_webrtc_session_url(session), as: :json

      assert_response :unauthorized
    end

    test "show returns pending status before answer" do
      sign_in @user
      session = WebrtcSession.create!(user: @user, offer: @valid_offer)

      get api_webrtc_session_url(session), as: :json

      assert_response :success
      json = JSON.parse(response.body)
      assert_equal "pending", json["status"]
      assert_nil json["answer"]
    end

    test "show returns answer when available" do
      sign_in @user
      session = WebrtcSession.create!(user: @user, offer: @valid_offer)
      session.set_answer!(@valid_answer.stringify_keys)

      get api_webrtc_session_url(session), as: :json

      assert_response :success
      json = JSON.parse(response.body)
      assert_equal "answered", json["status"]
      assert_equal @valid_answer.stringify_keys, json["answer"]
    end

    test "show returns gone for expired session" do
      sign_in @user
      session = WebrtcSession.create!(user: @user, offer: @valid_offer, expires_at: 1.minute.ago)

      get api_webrtc_session_url(session), as: :json

      assert_response :gone
      json = JSON.parse(response.body)
      assert_equal "expired", json["status"]
    end

    test "show returns not found for other user's session" do
      sign_in @other_user
      session = WebrtcSession.create!(user: @user, offer: @valid_offer)

      get api_webrtc_session_url(session), as: :json

      assert_response :not_found
    end

    # CLI posts answer (requires API key auth)
    test "update requires API key" do
      session = WebrtcSession.create!(user: @user, offer: @valid_offer)

      patch api_webrtc_session_url(session), params: { answer: @valid_answer }, as: :json

      assert_response :unauthorized
    end

    test "update with valid answer sets answer on session" do
      # Set API key on user
      @user.generate_api_key
      @user.save!

      session = WebrtcSession.create!(user: @user, offer: @valid_offer)

      patch api_webrtc_session_url(session),
            params: { answer: @valid_answer },
            headers: { "X-API-Key" => @user.api_key },
            as: :json

      assert_response :success
      json = JSON.parse(response.body)
      assert json["success"]
      assert_equal "answered", json["status"]

      session.reload
      assert_equal @valid_answer.stringify_keys, session.answer
      assert_equal "answered", session.status
    end

    test "update returns unauthorized for different user" do
      @user.generate_api_key
      @user.save!
      @other_user.generate_api_key
      @other_user.save!

      session = WebrtcSession.create!(user: @user, offer: @valid_offer)

      # Try to update with other user's API key
      patch api_webrtc_session_url(session),
            params: { answer: @valid_answer },
            headers: { "X-API-Key" => @other_user.api_key },
            as: :json

      assert_response :unauthorized
    end

    test "update returns gone for expired session" do
      @user.generate_api_key
      @user.save!

      session = WebrtcSession.create!(user: @user, offer: @valid_offer, expires_at: 1.minute.ago)

      patch api_webrtc_session_url(session),
            params: { answer: @valid_answer },
            headers: { "X-API-Key" => @user.api_key },
            as: :json

      assert_response :gone
    end
  end
end
