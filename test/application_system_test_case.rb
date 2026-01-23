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
    # Clear browser storage BEFORE super to prevent Signal session pollution between tests
    clear_browser_storage
    Warden.test_reset!
  end

  private

  def clear_browser_storage
    return unless page.driver.browser.respond_to?(:execute_script)

    page.execute_script(<<~JS)
      // Clear IndexedDB databases
      if (window.indexedDB) {
        indexedDB.databases().then(databases => {
          databases.forEach(db => {
            indexedDB.deleteDatabase(db.name);
          });
        }).catch(() => {});
      }

      // Clear localStorage and sessionStorage
      localStorage.clear();
      sessionStorage.clear();
    JS
  rescue => e
    # Don't fail test if cleanup fails
    Rails.logger.warn "[Test Cleanup] Failed to clear browser storage: #{e.message}"
  end
end
