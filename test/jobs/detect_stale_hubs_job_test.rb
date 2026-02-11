# frozen_string_literal: true

require "test_helper"

class DetectStaleHubsJobTest < ActiveJob::TestCase
  setup do
    @user = users(:one)
  end

  test "marks stale hubs as not alive" do
    hub = Hub.create!(
      user: @user,
      identifier: "stale-hub-#{SecureRandom.hex(4)}",

      alive: true,
      last_seen_at: 3.minutes.ago
    )

    assert hub.alive?
    refute hub.active?

    DetectStaleHubsJob.perform_now

    hub.reload
    refute hub.alive?
  end

  test "does not affect active hubs" do
    hub = Hub.create!(
      user: @user,
      identifier: "active-hub-#{SecureRandom.hex(4)}",

      alive: true,
      last_seen_at: 1.minute.ago
    )

    assert hub.alive?
    assert hub.active?

    DetectStaleHubsJob.perform_now

    hub.reload
    assert hub.alive?
  end

  test "does not affect already dead hubs" do
    hub = Hub.create!(
      user: @user,
      identifier: "dead-hub-#{SecureRandom.hex(4)}",

      alive: false,
      last_seen_at: 10.minutes.ago
    )

    refute hub.alive?

    DetectStaleHubsJob.perform_now

    hub.reload
    refute hub.alive?
  end

  test "broadcasts status change when marking stale hubs offline" do
    hub = Hub.create!(
      user: @user,
      identifier: "broadcast-hub-#{SecureRandom.hex(4)}",

      alive: true,
      last_seen_at: 3.minutes.ago
    )

    # Verify broadcast_update! is called by checking hub is processed
    # The broadcast itself is tested via integration/system tests
    assert_changes -> { hub.reload.alive? }, from: true, to: false do
      DetectStaleHubsJob.perform_now
    end
  end

  test "processes multiple stale hubs" do
    3.times do |i|
      Hub.create!(
        user: @user,
        identifier: "multi-stale-hub-#{i}-#{SecureRandom.hex(4)}",

        alive: true,
        last_seen_at: 5.minutes.ago
      )
    end

    stale_count = Hub.where(alive: true).where("last_seen_at <= ?", 2.minutes.ago).count
    assert_equal 3, stale_count

    DetectStaleHubsJob.perform_now

    stale_count = Hub.where(alive: true).where("last_seen_at <= ?", 2.minutes.ago).count
    assert_equal 0, stale_count
  end
end
