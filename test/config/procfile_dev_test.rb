# frozen_string_literal: true

require "test_helper"

class ProcfileDevTest < ActiveSupport::TestCase
  test "procfile dev does not require a machine-specific cloudflared tunnel" do
    procfile = Rails.root.join("Procfile.dev").read

    refute_includes procfile, "cloudflared tunnel run dev-laptop"
  end
end
