# frozen_string_literal: true

class CreateMemoryTool < ApplicationMCPTool
  tool_name "create-memory"
  description "Create a new memory with content, tags, and metadata. Memories are automatically embedded for semantic search."

  property :content, type: "string", description: "The content of the memory", required: true
  property :memory_type, type: "string", description: "Type of memory: fact, insight, code_snippet, summary, or other", required: false
  collection :tags, type: "string", description: "Array of tag names to associate with this memory", required: false
  property :metadata, type: "object", description: "Additional metadata as JSON object", required: false
  property :visibility, type: "string", description: "Visibility level: private, team, or public (default: private)", required: false
  property :source, type: "string", description: "Source of the memory (e.g., 'user_input', 'conversation', 'document')", required: false

  def perform
    user = current_user
    unless user
      report_error("Authentication required")
      return
    end

    # Build memory attributes
    memory_attrs = {
      content: content,
      user: user,
      memory_type: memory_type || "other",
      visibility: visibility || "private",
      metadata: metadata || {}
    }

    memory_attrs[:source] = source if source.present?
    memory_attrs[:team_id] = user.team_id if user.team_id.present?

    # Create the memory
    memory = Memory.new(memory_attrs)

    if memory.save
      # Handle tags if provided
      if tags.present? && tags.is_a?(Array)
        tags.each do |tag_name|
          tag = Tag.find_or_create_by(name: tag_name.to_s.strip.downcase)
          memory.tags << tag unless memory.tags.include?(tag)
        end
      end

      # Build response
      response_text = "Memory created successfully!\n\n"
      response_text += "ID: #{memory.id}\n"
      response_text += "Type: #{memory.memory_type}\n"
      response_text += "Visibility: #{memory.visibility}\n"
      response_text += "Tags: #{memory.tags.pluck(:name).join(', ')}\n" if memory.tags.any?
      response_text += "Content: #{memory.content.truncate(100)}\n"

      render(text: response_text)
    else
      report_error("Failed to create memory: #{memory.errors.full_messages.join(', ')}")
    end
  rescue => e
    report_error("Error creating memory: #{e.message}")
  end
end
