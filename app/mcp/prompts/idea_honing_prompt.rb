# frozen_string_literal: true

# Template for generating new prompts.
class IdeaHoningPrompt < ApplicationMCPPrompt
  # Set the prompt name.
  prompt_name "idea-honing"

  # Provide a user-facing description for your prompt.
  description "Helps set up the flow of what will be done"

  # Configure arguments (example structure — override as needed)
  argument :idea, description: "Idea", required: true

  # Optional: add more arguments if needed
  # argument :context, description: "Context for the input", default: ""

  # Optional: validations can be added as needed
  # validates :input, presence: true
  # validates :context, length: { maximum: 500 }

  # Main logic for prompt
  def perform
    render text: <<~TEXT
    Ask me one question at a time so we can develop a thorough, step-by-step spec for this idea. Each question should build on my previous answers, and our end goal is to have a detailed specification I can hand off to a developer. Let’s do this iteratively and dig into every relevant detail. Remember, only one question at a time.

    Here’s the idea:
    #{idea}
    TEXT
  end
end
