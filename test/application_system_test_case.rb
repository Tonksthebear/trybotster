require "test_helper"

class ApplicationSystemTestCase < ActionDispatch::SystemTestCase
  include Warden::Test::Helpers

  # Disable transactional tests - system tests spawn external processes (CLI)
  # that need to see committed data in the database
  self.use_transactional_tests = false

  driven_by :selenium, using: :headless_chrome, screen_size: [ 1400, 1400 ]

  setup do
    Warden.test_mode!
  end

  teardown do
    Warden.test_reset!
  end
end
