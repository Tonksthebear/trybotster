# frozen_string_literal: true

require "test_helper"

class ProcfileDevTest < ActiveSupport::TestCase
  test "procfile dev does not require a machine-specific cloudflared tunnel" do
    procfile = Rails.root.join("Procfile.dev").read

    refute_includes procfile, "cloudflared tunnel run dev-laptop"
  end

  test "procfile dev uses digest-based vite assets by default" do
    procfile = Rails.root.join("Procfile.dev").read

    assert_includes procfile, "bin/vite build --watch"
    assert_includes procfile, 'VITE_DEV_SERVER:-0'
  end
end
