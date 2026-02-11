# frozen_string_literal: true

require "test_helper"

module Integrations
  module Github
    module Webhooks
      class BaseHandlerTest < ActiveSupport::TestCase
        # BaseHandler methods are private, so we use a minimal subclass to expose them.
        # This avoids `send` gymnastics and makes the test intent clear.
        class TestableHandler < BaseHandler
          public :bot_author?, :mentioned_trybotster?, :create_cleanup_message
        end

        setup do
          @handler = TestableHandler.new({})
        end

        # =======================================================================
        # bot_author? — the infinite loop gate
        #
        # If this returns false for our own bot, we process our own comments
        # and spin forever. Every case here is load-bearing.
        # =======================================================================

        test "bot_author? returns true for trybotster" do
          assert @handler.bot_author?("trybotster")
        end

        test "bot_author? returns true for GitHub app bot suffix" do
          assert @handler.bot_author?("dependabot[bot]")
        end

        test "bot_author? returns true for trybotster app bot suffix" do
          assert @handler.bot_author?("trybotster[bot]")
        end

        test "bot_author? returns true for any bot suffix regardless of case" do
          assert @handler.bot_author?("SomeBot[BOT]")
          assert @handler.bot_author?("renovate[Bot]")
        end

        test "bot_author? returns false for normal usernames" do
          refute @handler.bot_author?("jasonconigliari")
          refute @handler.bot_author?("octocat")
        end

        test "bot_author? returns false for usernames containing bot without brackets" do
          refute @handler.bot_author?("robert")
          refute @handler.bot_author?("bottlecap")
          refute @handler.bot_author?("robotman")
        end

        test "bot_author? returns false for nil author" do
          refute @handler.bot_author?(nil)
        end

        test "bot_author? returns false for empty string" do
          refute @handler.bot_author?("")
        end

        # =======================================================================
        # mentioned_trybotster? — the trigger gate
        #
        # Must match @trybotster as a word boundary and be case-insensitive.
        # Must NOT match partial strings like @trybotster2 or email-like patterns.
        # =======================================================================

        test "mentioned_trybotster? detects @trybotster mention" do
          assert @handler.mentioned_trybotster?("Hey @trybotster please help")
        end

        test "mentioned_trybotster? detects @trybotster at start of text" do
          assert @handler.mentioned_trybotster?("@trybotster fix this")
        end

        test "mentioned_trybotster? detects @trybotster at end of text" do
          assert @handler.mentioned_trybotster?("please help @trybotster")
        end

        test "mentioned_trybotster? is case insensitive" do
          assert @handler.mentioned_trybotster?("Hey @TryBotster check this")
          assert @handler.mentioned_trybotster?("@TRYBOTSTER do something")
        end

        test "mentioned_trybotster? returns false when not mentioned" do
          refute @handler.mentioned_trybotster?("This is a normal comment")
        end

        test "mentioned_trybotster? returns false for nil text" do
          refute @handler.mentioned_trybotster?(nil)
        end

        test "mentioned_trybotster? returns false for empty string" do
          refute @handler.mentioned_trybotster?("")
        end

        test "mentioned_trybotster? returns false for partial match like trybotster without @" do
          refute @handler.mentioned_trybotster?("trybotster is cool")
        end

        # =======================================================================
        # create_cleanup_message — creates real Integrations::Github::Message records
        # =======================================================================

        test "create_cleanup_message creates an agent_cleanup message for an issue" do
          message = @handler.create_cleanup_message(
            repo: "owner/repo",
            number: 42,
            is_pr: false,
            reason: "issue_closed"
          )

          assert message.persisted?
          assert_equal "agent_cleanup", message.event_type
          assert_equal "owner/repo", message.repo
          assert_equal 42, message.issue_number
          assert_equal "issue_closed", message.payload["reason"]
          assert_equal false, message.payload["is_pr"]
          assert_equal "pending", message.status
        ensure
          message&.destroy
        end

        test "create_cleanup_message creates an agent_cleanup message for a PR" do
          message = @handler.create_cleanup_message(
            repo: "owner/repo",
            number: 99,
            is_pr: true,
            reason: "pr_closed"
          )

          assert message.persisted?
          assert_equal "agent_cleanup", message.event_type
          assert_equal 99, message.issue_number
          assert_equal true, message.payload["is_pr"]
          assert_equal "pr_closed", message.payload["reason"]
        ensure
          message&.destroy
        end

        test "create_cleanup_message payload includes repo and issue_number" do
          message = @handler.create_cleanup_message(
            repo: "test/repo",
            number: 7,
            is_pr: false,
            reason: "issue_closed"
          )

          assert_equal "test/repo", message.payload["repo"]
          assert_equal 7, message.payload["issue_number"]
        ensure
          message&.destroy
        end
      end
    end
  end
end
