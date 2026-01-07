# frozen_string_literal: true

class TunnelResponseStore
  # In-memory store with request_id => response
  # For production at scale: use Redis with TTL

  class << self
    def store
      @store ||= Concurrent::Map.new
    end

    def wait_for(request_id, timeout: 30)
      event = Concurrent::Event.new
      store[request_id] = { event: event, response: nil }

      if event.wait(timeout)
        store.delete(request_id)[:response]
      else
        store.delete(request_id)
        nil # Timeout
      end
    end

    def fulfill(request_id, response)
      if (entry = store[request_id])
        entry[:response] = response
        entry[:event].set
      end
    end
  end
end
