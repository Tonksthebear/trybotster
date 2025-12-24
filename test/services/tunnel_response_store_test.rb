# frozen_string_literal: true

require "test_helper"

class TunnelResponseStoreTest < ActiveSupport::TestCase
  setup do
    # Clear the store before each test
    TunnelResponseStore.instance_variable_set(:@store, nil)
  end

  test "wait_for returns nil on timeout" do
    request_id = SecureRandom.uuid

    # Use a very short timeout for testing
    result = TunnelResponseStore.wait_for(request_id, timeout: 0.1)

    assert_nil result
  end

  test "fulfill delivers response to waiting thread" do
    request_id = SecureRandom.uuid
    response_data = { "status" => 200, "body" => "Hello" }

    # Start waiting in a thread
    result = nil
    thread = Thread.new do
      result = TunnelResponseStore.wait_for(request_id, timeout: 5)
    end

    # Give the thread time to start waiting
    sleep 0.05

    # Fulfill the request
    TunnelResponseStore.fulfill(request_id, response_data)

    # Wait for the thread to complete
    thread.join

    assert_equal response_data, result
  end

  test "fulfill does nothing if no waiter" do
    request_id = SecureRandom.uuid
    response_data = { "status" => 200 }

    # This should not raise an error
    TunnelResponseStore.fulfill(request_id, response_data)

    # No waiter means nothing was stored - verify store is still empty for this request
    assert_nil TunnelResponseStore.store[request_id]
  end

  test "store is cleaned up after wait_for completes" do
    request_id = SecureRandom.uuid

    thread = Thread.new do
      TunnelResponseStore.wait_for(request_id, timeout: 0.1)
    end
    thread.join

    # The store entry should be removed
    assert_nil TunnelResponseStore.store[request_id]
  end

  test "store is cleaned up after successful fulfill" do
    request_id = SecureRandom.uuid
    response_data = { "status" => 200 }

    thread = Thread.new do
      TunnelResponseStore.wait_for(request_id, timeout: 5)
    end

    sleep 0.05
    TunnelResponseStore.fulfill(request_id, response_data)
    thread.join

    # The store entry should be removed
    assert_nil TunnelResponseStore.store[request_id]
  end

  test "multiple concurrent requests work independently" do
    request_id_1 = SecureRandom.uuid
    request_id_2 = SecureRandom.uuid
    response_1 = { "id" => 1 }
    response_2 = { "id" => 2 }

    results = {}

    thread1 = Thread.new do
      results[1] = TunnelResponseStore.wait_for(request_id_1, timeout: 5)
    end

    thread2 = Thread.new do
      results[2] = TunnelResponseStore.wait_for(request_id_2, timeout: 5)
    end

    sleep 0.05

    # Fulfill in reverse order
    TunnelResponseStore.fulfill(request_id_2, response_2)
    TunnelResponseStore.fulfill(request_id_1, response_1)

    thread1.join
    thread2.join

    assert_equal response_1, results[1]
    assert_equal response_2, results[2]
  end
end
