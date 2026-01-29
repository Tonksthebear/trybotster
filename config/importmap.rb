# Pin npm packages by running ./bin/importmap

pin "application"
pin "@rails/actioncable", to: "actioncable.esm.js"
pin "@hotwired/turbo-rails", to: "turbo.min.js"
pin "@hotwired/stimulus", to: "stimulus.min.js"
pin "@hotwired/stimulus-loading", to: "stimulus-loading.js"
pin "turbo_stream_update_attribute"
pin_all_from "app/javascript/controllers", under: "controllers"
pin_all_from "app/javascript/channels", under: "channels"
# Using esm.sh for proper ESM exports that work with importmaps
pin "@xterm/xterm", to: "https://esm.sh/@xterm/xterm@5.5.0"
pin "@xterm/addon-fit", to: "https://esm.sh/@xterm/addon-fit@0.10.0"

# Signal Protocol WASM for E2E encryption
pin "signal", to: "signal/index.js"
pin_all_from "app/javascript/workers", under: "workers"

# Connection management (global, Turbo-aware)
pin "connections", to: "connections/index.js"
pin_all_from "app/javascript/connections", under: "connections"

# Encrypted transport layer for preview
pin_all_from "app/javascript/transport", under: "transport"
pin_all_from "app/javascript/preview", under: "preview"
pin_all_from "app/javascript/channels", under: "channels"
pin "@tailwindplus/elements", to: "@tailwindplus--elements.js" # @1.0.22
