# Scheduler

Scheduling belongs in `muninn-gate-core`.

`PollScheduler<N>` is initialized from `GatewayConfig<N>`. It creates one schedule entry per enabled telemetry producer and tracks the next poll time in milliseconds. The scheduler does not sleep, spawn tasks, or know about timers.

Initial schedule entries are spread across the default polling interval. With the default 10 minute interval and three producers, the first producer is scheduled near startup, the second around 3.3 minutes later, and the third around 6.6 minutes later. Airtime per poll does not change, but spacing requests reduces bursty channel utilization.

The scheduler answers two questions:

- Which producers are due at `now_ms`?
- When is the next scheduled wake?

## Runtime Loop

The board runtime owns sleeping and task execution. On ESP32,
`muninn-gate-platform-esp32::scheduler::GatewayScheduler` runs inside the same
cooperative loop as WiFi/HTTP/smoltcp and the LoRa radio owner.

A typical board loop is:

```text
load config
validate selected board capabilities against config
create telemetry store from config
create poll scheduler from config and clock.now_ms()
loop:
  now = clock.now_ms()
  due = scheduler.due_producers(now)
  for each due producer:
    telemetry = meshcore_client.poll_producer(producer).await
    on success, update telemetry store and poll counters
    on failure, schedule retry or record final failure
    on final result, schedule next normal poll
  sleep until scheduler.next_wake_ms(), radio event, config event, or HTTP work
```

`poll_due_producers` captures the core behavior:

1. Call `MeshcoreClient::poll_producer`.
2. Normalize the timestamp if needed.
3. Update the `TelemetryStore`.
4. Record poll success or failure.
5. Schedule the next poll using the producer-specific interval or gateway default interval.

Retries are part of one scheduled producer poll. A failed attempt schedules a later retry instead of immediately looping. `poll_success_total` or `poll_failure_total` is incremented once after the retry policy reaches a final result. Lower-level retry transmissions still appear in radio counters.

HTTP `/poll` does not poll inside the socket handler. It records
an operator request. The scheduler runtime consumes that request, forces all
configured producers due, and emits serial/display updates after the pass.

## Policy

Scheduler policy should stay conservative:

- Poll only enabled telemetry producers.
- Use the per-producer interval override when present.
- Fall back to the gateway default interval.
- Space initial producer polls across the default interval.
- Apply deterministic per-producer jitter so restarts do not produce perfectly synchronized traffic.
- Use saturating counters for long-running devices.
- Treat failures as completed poll attempts so one failing producer does not starve the schedule.
- Keep retries bounded by `retry_count`; the default is five retries after the initial attempt.
- Schedule retries with a delay instead of retrying in a tight loop. The current core helper uses `jitter_secs` as the retry delay, with a 1 second floor when jitter is disabled.

Default timing is intentionally conservative:

- `default_interval_secs`: 600
- `min_interval_secs`: 60
- `max_interval_secs`: 3600
- `retry_count`: 5
- `jitter_secs`: 10

## Task Boundaries

The scheduler should not directly own radio access. It should drive the MeshCore client, and the MeshCore client should communicate with the radio owner.

This boundary keeps timing policy in core while leaving actual async runtime behavior to the platform and board crates. ESP32-S3 can use its chosen async primitives and task pinning. nRF52 can use a different runtime or a mostly serial event loop without changing scheduler semantics.
