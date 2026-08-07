# frozen_string_literal: true

require "test_helper"

class ProcfileDevTest < ActiveSupport::TestCase
  test "procfile dev does not require a machine-specific cloudflared tunnel" do
    procfile = Rails.root.join("Procfile.dev").read

    refute_includes procfile, "cloudflared tunnel run dev-laptop"
  end

  # Tailwind CLI exits after one build when stdin is closed (Foreman/non-TTY).
  # --watch=always keeps the process alive so foreman doesn't tear down the stack.
  # See: https://github.com/rails/tailwindcss-rails#building-for-production
  test "procfile dev keeps tailwind watch alive under foreman" do
    procfile = Rails.root.join("Procfile.dev").read

    assert_includes procfile, "tailwindcss:watch[always]"
  end
end
