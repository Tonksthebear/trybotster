# frozen_string_literal: true

module Integrations
  module Github
    module Webhooks
      # Creates Integrations::Github::Message records with properly formatted structured context
      class MessageCreator
        def initialize(params)
          @repo = params[:repo]
          @issue_number = params[:issue_number]
          @comment_id = params[:comment_id]
          @comment_body = params[:comment_body]
          @comment_author = params[:comment_author]
          @issue_title = params[:issue_title]
          @issue_body = params[:issue_body]
          @issue_url = params[:issue_url]
          @is_pr = params[:is_pr]
          @source_type = params[:source_type]
          @routed_info = params[:routed_info]
          @installation_id = params[:installation_id]
        end

        def call
          structured_context = build_structured_context
          formatted_context = format_structured_context(structured_context)

          message = Integrations::Github::Message.create!(
            event_type: "github_mention",
            repo: @repo,
            issue_number: @issue_number,
            payload: {
              prompt: formatted_context,
              structured_context: structured_context,
              repo: @repo,
              issue_number: @issue_number,
              comment_id: @comment_id,
              comment_body: @comment_body,
              comment_author: @comment_author,
              issue_title: @issue_title,
              issue_body: @issue_body,
              issue_url: @issue_url,
              is_pr: @is_pr,
              context: formatted_context,
              installation_id: @installation_id
            }
          )

          Rails.logger.info "Created Integrations::Github::Message #{message.id} for #{@repo}##{@issue_number}"
          message
        end

        private

        def build_structured_context
          owner, repo_name = @repo.split("/")

          if @routed_info
            build_routed_context(owner, repo_name)
          else
            build_direct_context(owner, repo_name)
          end
        end

        def build_routed_context(owner, repo_name)
          {
            source: {
              type: @source_type,
              repo: @repo,
              owner: owner,
              repo_name: repo_name,
              number: @routed_info[:source_number],
              comment_author: @comment_author
            },
            routed_to: {
              type: @routed_info[:target_type],
              number: @routed_info[:target_number],
              reason: @routed_info[:reason]
            },
            respond_to: {
              type: @routed_info[:source_type],
              number: @routed_info[:source_number],
              instruction: "Post your response as a comment on #{@routed_info[:source_type].upcase} ##{@routed_info[:source_number]}"
            },
            message: @comment_body,
            task: "Answer the question about the #{@routed_info[:source_type].upcase} changes",
            requirements: build_routed_requirements
          }
        end

        def build_direct_context(owner, repo_name)
          type = @is_pr ? "pr" : "issue"

          {
            source: {
              type: @source_type || (@is_pr ? "pr_comment" : "issue_comment"),
              repo: @repo,
              owner: owner,
              repo_name: repo_name,
              number: @issue_number,
              comment_author: @comment_author
            },
            routed_to: nil,
            respond_to: {
              type: type,
              number: @issue_number,
              instruction: "Post your response as a comment on #{type.upcase} ##{@issue_number}"
            },
            message: @comment_body,
            task: "Address the #{type} mention",
            requirements: build_direct_requirements(type)
          }
        end

        def build_routed_requirements
          requirements = {
            must_use_trybotster_mcp: true,
            fetch_first: @routed_info[:source_type],
            number_to_fetch: @routed_info[:source_number],
            context_number: @routed_info[:target_number]
          }

          if @routed_info[:source_type] == "pr"
            requirements[:must_include_closes_keyword] = closes_keyword_instruction
          end

          requirements.compact
        end

        def build_direct_requirements(type)
          requirements = {
            must_use_trybotster_mcp: true,
            fetch_first: type,
            number_to_fetch: @issue_number
          }

          unless @is_pr
            requirements[:must_include_closes_keyword] = closes_keyword_instruction
          end

          requirements.compact
        end

        def closes_keyword_instruction
          'If you are opening a PR AND you are closing an issue, you MUST use "Closes #{ISSUE_NUMBER}" in the description so that the PR is linked to the issue'
        end

        def format_structured_context(ctx)
          lines = []

          format_source_section(lines, ctx[:source])
          format_routing_section(lines, ctx[:routed_to])
          format_message_section(lines, ctx[:message])
          format_respond_to_section(lines, ctx[:respond_to])
          format_task_section(lines, ctx[:task])
          format_requirements_section(lines, ctx[:requirements])

          lines.join("\n")
        end

        def format_source_section(lines, source)
          return unless source

          lines << "## Source"
          lines << "Type: #{source[:type]}" if source[:type]
          lines << "Repository: #{source[:repo]}" if source[:repo]
          lines << "Number: ##{source[:number]}" if source[:number]
          lines << "Author: #{source[:comment_author]}" if source[:comment_author]
          lines << ""
        end

        def format_routing_section(lines, routed_to)
          return unless routed_to

          lines << "## Routing"
          lines << "Routed to: #{routed_to[:type]} ##{routed_to[:number]}"
          lines << "Reason: #{routed_to[:reason]}"
          lines << ""
        end

        def format_message_section(lines, message)
          return unless message

          lines << "## Message"
          lines << message
          lines << ""
        end

        def format_respond_to_section(lines, respond_to)
          return unless respond_to

          lines << "## Where to Respond"
          lines << "#{respond_to[:type].upcase} ##{respond_to[:number]}"
          lines << respond_to[:instruction]
          lines << ""
        end

        def format_task_section(lines, task)
          return unless task

          lines << "## Your Task"
          lines << task
          lines << ""
        end

        def format_requirements_section(lines, requirements)
          return unless requirements

          lines << "## Requirements"

          if requirements[:must_use_trybotster_mcp]
            lines << "- You MUST use ONLY the trybotster MCP server for ALL GitHub interactions"
          end

          if requirements[:fetch_first] && requirements[:number_to_fetch]
            lines << "- Start by fetching #{requirements[:fetch_first]} ##{requirements[:number_to_fetch]} details"
          end

          if requirements[:context_number]
            lines << "- You may fetch issue ##{requirements[:context_number]} for additional context if needed"
          end

          if requirements[:must_include_closes_keyword]
            lines << "- #{requirements[:must_include_closes_keyword]}"
          end
        end
      end
    end
  end
end
