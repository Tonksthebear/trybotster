# WebRTC P2P Design for CLI ↔ Browser Connection

## Overview

Allow users to view their running agents directly in the browser via peer-to-peer WebRTC connection. The Rails server only handles signaling (connection setup) - never sees actual agent data.

## Architecture

```
Browser                    Rails Server                  CLI (Hub)
   |                            |                           |
   |--POST /webrtc/sessions---->|  create pending session   |
   |   (SDP offer)              |                           |
   |                            |<---poll /bots/messages----|
   |                            |---webrtc_offer event----->|
   |                            |                           |
   |                            |<--PATCH /webrtc/sessions--|
   |                            |   (SDP answer)            |
   |<--GET /webrtc/sessions/:id-|                           |
   |   (poll for answer)        |                           |
   |                            |                           |
   |<================ WebRTC Data Channel =================>|
   |                    (Direct P2P)                        |
```

## Signaling Flow (Simple HTTP - No WebSocket)

### 1. Browser Initiates
```javascript
// Browser creates offer with all ICE candidates gathered
const pc = new RTCPeerConnection({
  iceServers: [{ urls: 'stun:stun.l.google.com:19302' }]
});
const channel = pc.createDataChannel('agents');
const offer = await pc.createOffer();
await pc.setLocalDescription(offer);

// Wait for ICE gathering to complete (vanilla ICE)
await new Promise(resolve => {
  if (pc.iceGatheringState === 'complete') resolve();
  else pc.onicegatheringchange = () => {
    if (pc.iceGatheringState === 'complete') resolve();
  };
});

// POST complete offer to server
const response = await fetch('/api/webrtc/sessions', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ offer: pc.localDescription })
});
const { session_id } = await response.json();
```

### 2. CLI Receives Offer
The CLI's existing message polling picks up a `webrtc_offer` event:
```rust
// In poll_messages, handle webrtc_offer event type
if event_type == "webrtc_offer" {
    let session_id = payload["session_id"].as_str();
    let offer_sdp = payload["offer"]["sdp"].as_str();
    self.handle_webrtc_offer(session_id, offer_sdp).await?;
}
```

### 3. CLI Creates Answer
```rust
use webrtc::api::APIBuilder;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::ice_transport::ice_server::RTCIceServer;

async fn handle_webrtc_offer(&mut self, session_id: &str, offer_sdp: &str) -> Result<()> {
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };

    let api = APIBuilder::new().build();
    let pc = api.new_peer_connection(config).await?;

    // Set up data channel handler
    pc.on_data_channel(Box::new(move |channel| {
        // Handle incoming data channel
        self.setup_agent_data_channel(channel);
    }));

    // Set remote description (the offer)
    let offer = RTCSessionDescription::offer(offer_sdp.to_owned())?;
    pc.set_remote_description(offer).await?;

    // Create answer
    let answer = pc.create_answer(None).await?;
    pc.set_local_description(answer.clone()).await?;

    // Wait for ICE gathering
    // ... (gather complete)

    // POST answer back to server
    self.client.patch(&format!("{}/api/webrtc/sessions/{}", self.server_url, session_id))
        .json(&json!({ "answer": pc.local_description() }))
        .send()?;

    // Store peer connection
    self.webrtc_peer = Some(pc);
    Ok(())
}
```

### 4. Browser Receives Answer
```javascript
// Poll for answer
const pollForAnswer = async (sessionId) => {
  while (true) {
    const resp = await fetch(`/api/webrtc/sessions/${sessionId}`);
    const data = await resp.json();
    if (data.answer) {
      await pc.setRemoteDescription(data.answer);
      return; // Connected!
    }
    await new Promise(r => setTimeout(r, 1000)); // Poll every second
  }
};
```

### 5. Data Channel Communication
Once connected, browser and CLI communicate directly:

```javascript
// Browser sends
channel.send(JSON.stringify({ type: 'get_agents' }));

// Browser receives
channel.onmessage = (event) => {
  const data = JSON.parse(event.data);
  if (data.type === 'agents_list') {
    updateAgentUI(data.agents);
  } else if (data.type === 'terminal_output') {
    appendToTerminal(data.output);
  }
};
```

## Rails Models/Controllers

### WebRTC Session Model
```ruby
# app/models/webrtc_session.rb
class WebrtcSession < ApplicationRecord
  belongs_to :user

  # Columns: user_id, offer (jsonb), answer (jsonb),
  #          status (pending/answered/connected/expired),
  #          expires_at, created_at, updated_at

  scope :pending, -> { where(status: 'pending') }
  scope :for_user, ->(user) { where(user: user) }
end
```

### API Controller
```ruby
# app/controllers/api/webrtc_sessions_controller.rb
module Api
  class WebrtcSessionsController < ApplicationController
    # Browser creates session with offer
    def create
      session = current_user.webrtc_sessions.create!(
        offer: params[:offer],
        status: 'pending',
        expires_at: 5.minutes.from_now
      )

      # Create message for CLI to pick up
      HubCommand.create!(
        event_type: 'webrtc_offer',
        payload: {
          session_id: session.id,
          offer: params[:offer],
          repo: params[:repo] # optional: filter by repo
        }
      )

      render json: { session_id: session.id }
    end

    # Browser polls for answer
    def show
      session = current_user.webrtc_sessions.find(params[:id])
      render json: {
        status: session.status,
        answer: session.answer
      }
    end

    # CLI posts answer
    def update
      session = WebrtcSession.find(params[:id])
      # Verify CLI owns this session via API key -> user match
      session.update!(answer: params[:answer], status: 'answered')
      render json: { success: true }
    end
  end
end
```

## Data Channel Protocol

Simple JSON messages over the data channel. Designed for read-only first, with interactive mode as easy addition.

### Browser → CLI

**Phase 1 (Read-only):**
```json
{ "type": "get_agents" }
{ "type": "subscribe", "agent_id": "repo-issue-123" }
{ "type": "unsubscribe", "agent_id": "repo-issue-123" }
```

**Phase 2 (Interactive) - Just add this one handler:**
```json
{ "type": "input", "agent_id": "repo-issue-123", "data": "ls -la\n" }
```

### CLI → Browser
```json
{ "type": "agents", "agents": [
  { "id": "repo-issue-123", "repo": "owner/repo", "issue": 123, "status": "running" }
]}
{ "type": "output", "agent_id": "repo-issue-123", "data": "base64-encoded-terminal-data" }
{ "type": "status", "agent_id": "repo-issue-123", "status": "completed" }
{ "type": "error", "message": "Agent not found" }
```

### Why Base64 for Terminal Output?

Terminal output contains ANSI escape codes, control characters, and potentially binary data. Base64 ensures clean JSON transport. Browser decodes before passing to xterm.js.

## Rust CLI: Message Handler Architecture

Design the handler to make adding `input` trivial:

```rust
// cli/src/webrtc_handler.rs

pub struct WebRTCHandler {
    agents: Arc<Mutex<HashMap<String, Agent>>>,
    subscriptions: HashSet<String>,  // agent_ids browser is subscribed to
}

impl WebRTCHandler {
    /// Handle incoming message from browser
    pub async fn handle_message(&mut self, msg: &str, dc: &RTCDataChannel) -> Result<()> {
        let message: BrowserMessage = serde_json::from_str(msg)?;

        match message {
            BrowserMessage::GetAgents => {
                self.send_agents_list(dc).await?;
            }
            BrowserMessage::Subscribe { agent_id } => {
                self.subscriptions.insert(agent_id);
            }
            BrowserMessage::Unsubscribe { agent_id } => {
                self.subscriptions.remove(&agent_id);
            }
            // Phase 2: Just add this match arm
            // BrowserMessage::Input { agent_id, data } => {
            //     self.send_to_agent_pty(&agent_id, &data).await?;
            // }
        }
        Ok(())
    }

    /// Stream terminal output for subscribed agents
    pub async fn stream_output(&self, agent_id: &str, data: &[u8], dc: &RTCDataChannel) {
        if self.subscriptions.contains(agent_id) {
            let msg = CLIMessage::Output {
                agent_id: agent_id.to_string(),
                data: base64::encode(data),
            };
            dc.send_text(serde_json::to_string(&msg).unwrap()).await.ok();
        }
    }

    // Phase 2: Just add this method
    // async fn send_to_agent_pty(&self, agent_id: &str, data: &str) -> Result<()> {
    //     if let Some(agent) = self.agents.lock().await.get_mut(agent_id) {
    //         agent.pty_writer.write_all(data.as_bytes())?;
    //     }
    //     Ok(())
    // }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BrowserMessage {
    GetAgents,
    Subscribe { agent_id: String },
    Unsubscribe { agent_id: String },
    // Phase 2: Just add this variant
    // Input { agent_id: String, data: String },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CLIMessage {
    Agents { agents: Vec<AgentInfo> },
    Output { agent_id: String, data: String },
    Status { agent_id: String, status: String },
    Error { message: String },
}
```

## Browser: Terminal Component Architecture

Use xterm.js with write-only mode initially, trivial to enable input:

```typescript
// web/components/AgentTerminal.tsx

import { Terminal } from 'xterm';
import { FitAddon } from 'xterm-addon-fit';

interface AgentTerminalProps {
  agentId: string;
  dataChannel: RTCDataChannel;
  interactive?: boolean;  // Phase 2: flip this to true
}

export function AgentTerminal({ agentId, dataChannel, interactive = false }: AgentTerminalProps) {
  const terminalRef = useRef<HTMLDivElement>(null);
  const xtermRef = useRef<Terminal>();

  useEffect(() => {
    const term = new Terminal({
      cursorBlink: interactive,  // Only blink cursor in interactive mode
      disableStdin: !interactive, // Phase 1: disable input
    });
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(terminalRef.current!);
    fitAddon.fit();
    xtermRef.current = term;

    // Subscribe to this agent's output
    dataChannel.send(JSON.stringify({ type: 'subscribe', agent_id: agentId }));

    // Receive output
    const handleMessage = (event: MessageEvent) => {
      const msg = JSON.parse(event.data);
      if (msg.type === 'output' && msg.agent_id === agentId) {
        term.write(atob(msg.data));  // Decode base64
      }
    };
    dataChannel.addEventListener('message', handleMessage);

    // Phase 2: Just remove the `if` wrapper when ready
    if (interactive) {
      term.onData((data) => {
        dataChannel.send(JSON.stringify({
          type: 'input',
          agent_id: agentId,
          data: data
        }));
      });
    }

    return () => {
      dataChannel.send(JSON.stringify({ type: 'unsubscribe', agent_id: agentId }));
      dataChannel.removeEventListener('message', handleMessage);
      term.dispose();
    };
  }, [agentId, dataChannel, interactive]);

  return <div ref={terminalRef} className="h-full w-full" />;
}
```

## Enabling Interactive Mode (Phase 2)

When ready to enable input, the changes are:

### CLI (3 lines)
```rust
// Uncomment in handle_message:
BrowserMessage::Input { agent_id, data } => {
    self.send_to_agent_pty(&agent_id, &data).await?;
}

// Uncomment send_to_agent_pty method
```

### Browser (1 prop)
```tsx
<AgentTerminal agentId={id} dataChannel={dc} interactive={true} />
```

That's it. The architecture supports both from day one.

## Rust Dependencies

Add to `cli/Cargo.toml`:
```toml
[dependencies]
webrtc = "0.11"  # Pure Rust WebRTC
```

## Connection Failure Handling

If P2P connection fails (symmetric NAT, strict firewall):

```javascript
pc.oniceconnectionstatechange = () => {
  if (pc.iceConnectionState === 'failed') {
    showError("Direct connection not possible. Your network may block peer-to-peer connections.");
  }
};

// Also timeout after 30 seconds of trying
setTimeout(() => {
  if (pc.iceConnectionState !== 'connected') {
    pc.close();
    showError("Could not establish direct connection.");
  }
}, 30000);
```

## Security Considerations

1. **Session expiry**: WebRTC sessions expire after 5 minutes if not answered
2. **User binding**: Sessions are bound to authenticated user
3. **No data through server**: Only SDP signaling passes through Rails
4. **DTLS encryption**: WebRTC data channels are encrypted by default

## Implementation Phases

### Phase 1: Basic Connection
- [ ] Add `webrtc` crate to CLI
- [ ] Create WebrtcSession model and migration
- [ ] Add signaling API endpoints
- [ ] Implement CLI WebRTC offer handling
- [ ] Basic browser connection test

### Phase 2: Agent Streaming
- [ ] Define data channel protocol
- [ ] Stream agent list to browser
- [ ] Stream terminal output for subscribed agents
- [ ] Handle agent input from browser

### Phase 3: Web UI
- [ ] Build agents dashboard page
- [ ] Terminal viewer component
- [ ] Connection status indicators
- [ ] Error handling UI
