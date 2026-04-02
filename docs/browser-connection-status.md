# Browser Connection Status

The hub page shows three independent badges:

- Browser: this tab's ActionCable socket to Rails
- WebRTC: the browser <-> hub peer connection
- Hub: the hub's health as reported through Rails

## Ownership

- Browser badge must be driven only by direct ActionCable socket state.
- Hub badge must be driven only by live hub health messages.
- WebRTC badge must be driven only by peer/subscription state.

Do not derive one badge from another. In particular:

- browser connected does not imply hub online
- hub online does not imply WebRTC connected
- WebRTC failure must not rewrite browser or hub badges

## Gate

The browser should attempt WebRTC only when both of these are true:

- the browser socket to Rails is `connected`
- the hub health is `online`

If either prerequisite drops while WebRTC is disconnected, the middle badge
should stay disconnected. When both prerequisites become true again, WebRTC
should retry automatically.

## Implementation Notes

- [connection_status_controller.js](../app/javascript/controllers/connection_status_controller.js) renders the badges.
- [hub_signaling_client.js](../app/javascript/transport/hub_signaling_client.js) is the source of browser socket truth.
- [hub_route.js](../app/javascript/connections/hub_route.js) applies the WebRTC attempt gate.
- [webrtc_connection_test.rb](../test/system/webrtc_connection_test.rb) proves the gate and SharedWorker bootstrap end to end.

## WAN Timing

For WAN connections, the answer must go back to the browser as soon as it is
created. Queued browser ICE candidates are secondary.

Two practical rules came out of debugging:

- browser mDNS host candidates like `*.local` must not stall the CLI if Rust
  cannot resolve or parse them
- the CLI should send the encrypted answer before it spends time applying any
  queued browser ICE candidates

Otherwise the browser can appear to take a fixed `~5s` to connect even though
the answer itself was created in a few milliseconds.
