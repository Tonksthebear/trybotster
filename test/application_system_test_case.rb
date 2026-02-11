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
      var done = arguments[arguments.length - 1];

      localStorage.clear();
      sessionStorage.clear();

      // Step 1: Tell SharedWorker to clear all sessions + close its IDB connection.
      // Must complete BEFORE deleting IDB, otherwise deleteDatabase gets blocked.
      function clearWorker() {
        return new Promise(function(resolve) {
          try {
            var workerUrl = document.querySelector('meta[name="crypto-worker-url"]');
            if (!workerUrl || !workerUrl.content) { resolve(); return; }
            var worker = new SharedWorker(workerUrl.content, { type: "module", name: "vodozemac-crypto" });
            worker.port.onmessage = function(e) { if (e.data.id === 999999) resolve(); };
            worker.port.start();
            worker.port.postMessage({ id: 999999, action: "clearAllSessions" });
            setTimeout(resolve, 3000);
          } catch(e) { resolve(); }
        });
      }

      // Step 2: Delete IndexedDB databases (SharedWorker closed its connection in step 1).
      function clearIDB() {
        return new Promise(function(resolve) {
          try {
            if (window.indexedDB && indexedDB.databases) {
              indexedDB.databases().then(function(databases) {
                return Promise.all(
                  databases.map(function(db) {
                    return new Promise(function(r) {
                      var req = indexedDB.deleteDatabase(db.name);
                      req.onsuccess = r;
                      req.onerror = r;
                      req.onblocked = r;
                    });
                  })
                );
              }).then(resolve).catch(resolve);
            } else { resolve(); }
          } catch(e) { resolve(); }
        });
      }

      clearWorker().then(clearIDB).then(done);
    JS

    # Navigate to about:blank to destroy all JS singletons
    # (ConnectionManager, bridge, etc.) between tests.
    # SharedWorkers persist across navigations in the same origin,
    # but module-level singletons are destroyed on full page unload.
    page.driver.browser.navigate.to("about:blank")
  rescue => e
    # Don't fail test if cleanup fails
    Rails.logger.warn "[Test Cleanup] Failed to clear browser storage: #{e.message}"
  end
end
