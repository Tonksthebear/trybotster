# frozen_string_literal: true

LangchainrbRails.configure do |config|
  config.vectorsearch = Langchain::Vectorsearch::Pgvector.new(
    llm: Langchain::LLM::GoogleGemini.new(api_key: ENV["GOOGLE_GEMINI_API_KEY"])
  )
end
