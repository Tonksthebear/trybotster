require "test_helper"

class ApplicationSystemTestCase < ActionDispatch::SystemTestCase
  include Warden::Test::Helpers

  # Disable transactional tests - system tests spawn external processes (CLI)
  # that need to see committed data in the database
  self.use_transactional_tests = false

  driven_by :selenium, using: :headless_chrome, screen_size: [ 1400, 1400 ]

  setup do
    Warden.test_mode!

    # Stub TURN credential API to avoid WebMock blocking external requests.
    # Returns a minimal TURN server config that WebRTC can use.
    stub_request(:get, /metered\.live\/api\/v1\/turn\/credentials/)
      .to_return(
        status: 200,
        body: [
          { urls: "turn:global.relay.metered.ca:443?transport=tcp", username: "test", credential: "test" }
        ].to_json,
        headers: { "Content-Type" => "application/json" }
      )
  end

  teardown do
    # Clear browser storage BEFORE super to prevent Signal session pollution between tests
    clear_browser_storage
    Warden.test_reset!
  end

  private

  def clear_browser_storage
    return unless page.driver.browser.respond_to?(:execute_script)

    # Clear crypto SharedWorker in-memory sessions + IndexedDB + localStorage.
    # The SharedWorker persists across page navigations in the same browser.
    # Without clearing its in-memory sessions Map, hasSession() returns true
    # for hubs from previous tests, preventing "unpaired" state transitions.
    page.driver.browser.execute_async_script(<<~JS)
      const done = arguments[arguments.length - 1];

      localStorage.clear();
      sessionStorage.clear();

      // Send clearAllSessions directly to the crypto SharedWorker.
      // We open a fresh port to the same named SharedWorker and send the
      // command. This works even if the page's bridge was never initialized.
      const clearWorker = new Promise((resolve) => {
        try {
          const worker = new SharedWorker(
            document.querySelector('meta[name="crypto-worker-url"]')?.content,
            { type: "module", name: "vodozemac-crypto" }
          );
          worker.port.onmessage = (e) => {
            if (e.data.id === 999999) resolve();
          };
          worker.port.start();
          worker.port.postMessage({ id: 999999, action: "clearAllSessions" });
          // Timeout safety net
          setTimeout(resolve, 2000);
        } catch { resolve(); }
      });

      // Also clear IndexedDB as a safety net
      const clearIDB = new Promise((resolve) => {
        if (window.indexedDB && indexedDB.databases) {
          indexedDB.databases().then(databases => {
            return Promise.all(
              databases.map(db => new Promise((r) => {
                const req = indexedDB.deleteDatabase(db.name);
                req.onsuccess = r;
                req.onerror = r;
                req.onblocked = r;
              }))
            );
          }).then(resolve).catch(resolve);
        } else {
          resolve();
        }
      });

      Promise.all([clearWorker, clearIDB]).then(() => done());
    JS
  rescue => e
    # Don't fail test if cleanup fails
    Rails.logger.warn "[Test Cleanup] Failed to clear browser storage: #{e.message}"
  end
end
