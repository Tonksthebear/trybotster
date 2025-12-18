# frozen_string_literal: true

require "test_helper"

class WebrtcSessionTest < ActiveSupport::TestCase
  setup do
    @user = users(:one)
    @valid_offer = { "type" => "offer", "sdp" => "v=0\r\no=- 123 2 IN IP4 127.0.0.1\r\n..." }
    @valid_answer = { "type" => "answer", "sdp" => "v=0\r\no=- 456 2 IN IP4 127.0.0.1\r\n..." }
  end

  # Validation tests
  test "valid session with required attributes" do
    session = WebrtcSession.new(
      user: @user,
      offer: @valid_offer,
      expires_at: 5.minutes.from_now
    )
    assert session.valid?
  end

  test "invalid without user" do
    session = WebrtcSession.new(
      offer: @valid_offer,
      expires_at: 5.minutes.from_now
    )
    assert_not session.valid?
    assert_includes session.errors[:user], "must exist"
  end

  test "invalid without offer" do
    session = WebrtcSession.new(
      user: @user,
      expires_at: 5.minutes.from_now
    )
    assert_not session.valid?
    assert_includes session.errors[:offer], "can't be blank"
  end

  test "defaults status to pending" do
    session = WebrtcSession.new(
      user: @user,
      offer: @valid_offer
    )
    session.valid? # trigger before_validation
    assert_equal "pending", session.status
  end

  test "defaults expires_at to 5 minutes from now" do
    session = WebrtcSession.new(
      user: @user,
      offer: @valid_offer
    )
    session.valid? # trigger before_validation
    assert_in_delta 5.minutes.from_now, session.expires_at, 5.seconds
  end

  test "validates status inclusion" do
    session = WebrtcSession.new(
      user: @user,
      offer: @valid_offer,
      status: "invalid_status"
    )
    assert_not session.valid?
    assert_includes session.errors[:status], "invalid_status is not a valid status"
  end

  # Instance method tests
  test "set_answer! updates answer and status" do
    session = WebrtcSession.create!(
      user: @user,
      offer: @valid_offer
    )

    session.set_answer!(@valid_answer)

    assert_equal @valid_answer, session.answer
    assert_equal "answered", session.status
    assert session.answered?
  end

  test "mark_connected! updates status" do
    session = WebrtcSession.create!(
      user: @user,
      offer: @valid_offer
    )
    session.set_answer!(@valid_answer)

    session.mark_connected!

    assert_equal "connected", session.status
    assert session.connected?
  end

  test "mark_expired! updates status" do
    session = WebrtcSession.create!(
      user: @user,
      offer: @valid_offer
    )

    session.mark_expired!

    assert_equal "expired", session.status
  end

  test "expired? returns true when past expires_at" do
    session = WebrtcSession.create!(
      user: @user,
      offer: @valid_offer,
      expires_at: 1.minute.ago
    )

    assert session.expired?
  end

  test "expired? returns false when before expires_at" do
    session = WebrtcSession.create!(
      user: @user,
      offer: @valid_offer,
      expires_at: 5.minutes.from_now
    )

    assert_not session.expired?
  end

  # Scope tests
  test "pending scope returns only pending sessions" do
    WebrtcSession.create!(user: @user, offer: @valid_offer, status: "pending")
    WebrtcSession.create!(user: @user, offer: @valid_offer, status: "answered")

    pending_sessions = WebrtcSession.pending

    assert_equal 1, pending_sessions.count
    assert pending_sessions.all? { |s| s.status == "pending" }
  end

  test "active scope returns pending, answered, and connected sessions" do
    WebrtcSession.create!(user: @user, offer: @valid_offer, status: "pending")
    WebrtcSession.create!(user: @user, offer: @valid_offer, status: "answered")
    WebrtcSession.create!(user: @user, offer: @valid_offer, status: "expired")

    active_sessions = WebrtcSession.active

    assert_equal 2, active_sessions.count
    assert active_sessions.all? { |s| %w[pending answered connected].include?(s.status) }
  end

  test "for_user scope filters by user" do
    other_user = users(:two)
    WebrtcSession.create!(user: @user, offer: @valid_offer)
    WebrtcSession.create!(user: other_user, offer: @valid_offer)

    user_sessions = WebrtcSession.for_user(@user)

    assert_equal 1, user_sessions.count
    assert_equal @user.id, user_sessions.first.user_id
  end
end
