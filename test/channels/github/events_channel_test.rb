# frozen_string_literal: true

require "test_helper"
require "minitest/mock"

module Github
  class EventsChannelTest < ActionCable::Channel::TestCase
    tests Github::EventsChannel

    setup do
      @user = users(:jason)
      @test_repo = "botster/trybotster"
      stub_connection current_user: @user
    end

    # === Subscription Tests ===

    test "subscribes with valid repo and streams from github events channel" do
      stub_github_access(success: true) do
        subscribe repo: @test_repo

        assert subscription.confirmed?
        assert_has_stream "github_events:#{@test_repo}"
      end
    end

    test "rejects subscription without repo" do
      subscribe

      assert subscription.rejected?
    end

    test "rejects subscription with blank repo" do
      subscribe repo: ""

      assert subscription.rejected?
    end

    test "rejects subscription when github access validation fails" do
      stub_github_access(success: false) do
        subscribe repo: @test_repo

        assert subscription.rejected?
      end
    end

    test "rejects subscription when user has no github app token" do
      @user.define_singleton_method(:github_app_token) { nil }
      subscribe repo: @test_repo

      assert subscription.rejected?
    end

    # === Replay Tests ===

    test "replays pending messages on subscribe" do
      msg = Integrations::Github::Message.create!(
        event_type: "github_mention",
        repo: @test_repo,
        issue_number: 42,
        payload: { repo: @test_repo, issue_number: 42 }
      )

      stub_github_access(success: true) do
        subscribe repo: @test_repo

        assert subscription.confirmed?
        assert_equal 1, transmissions.size
        assert_equal msg.id, transmissions.first["id"]
        assert_equal "github_mention", transmissions.first["event_type"]
        assert_nil transmissions.first["sequence"] # No sequence on github channel
      end
    end

    test "does not replay acknowledged messages" do
      msg = Integrations::Github::Message.create!(
        event_type: "github_mention",
        repo: @test_repo,
        issue_number: 42,
        payload: { repo: @test_repo, issue_number: 42 }
      )

      Github::App.stub :create_issue_reaction, { success: true } do
        msg.acknowledge!
      end

      stub_github_access(success: true) do
        subscribe repo: @test_repo

        assert subscription.confirmed?
        assert_equal 0, transmissions.size
      end
    end

    test "only replays messages for subscribed repo" do
      msg_matching = Integrations::Github::Message.create!(
        event_type: "github_mention",
        repo: @test_repo,
        issue_number: 1,
        payload: { repo: @test_repo, issue_number: 1 }
      )
      msg_other = Integrations::Github::Message.create!(
        event_type: "github_mention",
        repo: "other/repo",
        issue_number: 2,
        payload: { repo: "other/repo", issue_number: 2 }
      )

      stub_github_access(success: true) do
        subscribe repo: @test_repo

        assert subscription.confirmed?
        assert_equal 1, transmissions.size
        assert_equal msg_matching.id, transmissions.first["id"]
      end
    end

    # === Ack Tests ===

    test "ack action acknowledges a github message" do
      msg = Integrations::Github::Message.create!(
        event_type: "github_mention",
        repo: @test_repo,
        issue_number: 42,
        payload: { repo: @test_repo, issue_number: 42 }
      )

      stub_github_access(success: true) do
        subscribe repo: @test_repo
        assert subscription.confirmed?

        Github::App.stub :create_issue_reaction, { success: true } do
          perform :ack, id: msg.id
        end
      end

      msg.reload
      assert msg.acknowledged?
    end

    test "ack action is idempotent for already acknowledged messages" do
      msg = Integrations::Github::Message.create!(
        event_type: "github_mention",
        repo: @test_repo,
        issue_number: 42,
        payload: { repo: @test_repo, issue_number: 42 }
      )

      Github::App.stub :create_issue_reaction, { success: true } do
        msg.acknowledge!
      end

      stub_github_access(success: true) do
        subscribe repo: @test_repo
        assert subscription.confirmed?

        # Should not raise
        perform :ack, id: msg.id
      end

      msg.reload
      assert msg.acknowledged?
    end

    private

    def stub_github_access(success:)
      installation_result = { success: success }
      @user.define_singleton_method(:github_app_token) { "fake-token" }
      Github::App.stub :get_installation_for_repo, installation_result do
        yield
      end
    end
  end
end
