# frozen_string_literal: true

class FetchMemoriesTool < ApplicationMCPTool
  tool_name "fetch-memories"
  description "Search memories using semantic search. Finds similar memories based on query text with optional filtering by type, tags, and visibility."

  property :query, type: "string", description: "Search query text for semantic similarity search", required: true
  property :limit, type: "integer", description: "Maximum number of results to return (default: 5)", required: false
  property :threshold, type: "number", description: "Similarity threshold (0.0-1.0, default: 0.7)", required: false
  property :memory_type, type: "string", description: "Filter by memory type: fact, insight, code_snippet, summary, or other", required: false
  collection :tags, type: "string", description: "Filter by tag names (matches memories with any of these tags)", required: false
  property :visibility, type: "string", description: "Filter by visibility: private, team, or public", required: false

  def perform
    user = current_user
    unless user
      report_error("Authentication required")
      return
    end

    # Build search options
    options = {}
    options[:limit] = limit.to_i if limit.present? && limit.to_i > 0
    options[:threshold] = threshold.to_f if threshold.present?
    options[:memory_type] = memory_type if memory_type.present?
    options[:tags] = tags if tags.present? && tags.is_a?(Array)

    # Handle visibility filter
    if visibility.present?
      options[:visibility] = [visibility]
    end

    # Perform semantic search
    memories = Memory.similarity_search([query, user, options].join(" "))

    if memories.empty?
      render(text: "No memories found matching your query.")
      return
    end

    # Build response
    response_text = "Found #{memories.length} matching #{'memory'.pluralize(memories.length)}:\n\n"

    memories.each_with_index do |memory, index|
      response_text += "#{index + 1}. "
      response_text += "[#{memory.memory_type}] " if memory.memory_type
      response_text += "#{memory.content.truncate(150)}\n"

      if memory.tags.any?
        response_text += "   Tags: #{memory.tags.pluck(:name).join(', ')}\n"
      end

      if memory.metadata.present?
        response_text += "   Metadata: #{memory.metadata.to_json}\n"
      end

      response_text += "   Created: #{memory.created_at.strftime('%Y-%m-%d %H:%M')}\n"
      response_text += "   ID: #{memory.id}\n"
      response_text += "\n"
    end

    render(text: response_text)
  rescue => e
    report_error("Error searching memories: #{e.message}")
  end
end
