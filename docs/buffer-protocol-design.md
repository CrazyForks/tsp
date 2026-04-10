# TSP Intermediary Buffer Protocol

## Overview

A message-level protocol between TSP clients and their intermediary buffer.
Operates on top of TSP — sequence numbers and acks are TSP payload data,
not transport concerns. SSE/WebSocket/HTTP are just delivery mechanisms.

## Problem

When a client disconnects and reconnects (restart, network change, session end),
the intermediary replays all buffered messages. Without a protocol for tracking
what the client has already received, the client gets flooded with duplicates.

## Protocol

### Message Delivery

P assigns a monotonic sequence number to each message it buffers per recipient.

```
P buffers message for Bob:
  sequence: 42
  payload: <encrypted TSP message>
  timestamp: <server time>
  expires_at: <timestamp + TTL>
```

When delivering (via SSE, WebSocket, or any transport), P includes the sequence:

```
SSE event:
  id: 42
  data: <CESR-T encoded payload>
```

The sequence number is per-recipient and monotonically increasing.
It is NOT a message ID — it is a buffer position for this recipient's queue.

### Client Acknowledgment

After the client processes received messages, it sends a cumulative ack:

```
Client -> P:  ack(up_to_sequence: 42)
```

This means: "I have received and processed all messages up to and including
sequence 42. You may delete them."

P deletes all messages with sequence <= 42 from this recipient's buffer.

The client acks periodically — not necessarily after every message. A reasonable
strategy: ack after every batch of received messages, or after a short delay
(e.g., 1 second) to batch multiple messages into one ack.

### Client Resume

On connect (or reconnect), the client tells P where to resume:

```
Client -> P:  resume(since_sequence: 42)
```

P replays all messages with sequence > 42.

If the client has no stored sequence (first connect, state loss), it sends
`since_sequence: 0` and receives everything in the buffer.

### Client-Side Persistence

The client persists `last_acked_sequence` to local storage.
This is application state — the SDK does not persist it.

- tspchat: file in agent home directory
- teagent gateway: file in agent home directory (same agent, same cursor)

On startup, the client reads the persisted value and uses it as `since_sequence`.

### Transport Mapping

The protocol is transport-agnostic. How it maps to SSE:

**Delivery**: sequence number = SSE event `id` field.
The SSE client library tracks it internally for within-session reconnects.

**Resume**: `since_sequence` = `Last-Event-ID` HTTP header on initial GET.
The client sets this from its persisted `last_acked_sequence`.

**Ack**: HTTP POST to an ack endpoint on P.
```
POST /ack/{recipient_did}
Content-Type: application/json
Body: { "up_to_sequence": 42 }
```

Alternatively, the ack can be a TSP message sent to P's DID.

## Buffer Management at P

```
Per-recipient queue:
  - Messages ordered by sequence number (monotonic)
  - Deleted when: acked by client OR TTL expires
  - Max queue depth: configurable (default 2000)
  - TTL: configurable (default 7 days)
  - Overflow: oldest messages evicted when max depth exceeded
```

### Deletion Priority

1. Client ack — immediate deletion of acked messages
2. TTL expiry — periodic cleanup of expired messages
3. Overflow — oldest-first eviction when queue exceeds max depth

### Sequence Number Properties

- Monotonically increasing per recipient
- Starts at 0 for a new recipient
- Never reused (even after deletion)
- Survives P restarts (persisted with the buffer)
  - For in-memory buffers: sequence resets on restart, client handles via dedup
  - For persistent buffers (future): sequence survives restart

## Client Deduplication

At-least-once delivery means duplicates are possible:
- P replays messages on reconnect that the client already processed
  but hadn't acked yet (crash between process and ack)
- Network issues cause retransmission

The client deduplicates by tracking `last_processed_sequence`:
- If received sequence <= last_processed_sequence: skip
- If received sequence > last_processed_sequence: process, update tracker

`last_processed_sequence` and `last_acked_sequence` may differ:
- `last_processed_sequence`: updated immediately on receive
- `last_acked_sequence`: updated when ack is sent (may be batched/delayed)

On restart, the client uses `last_acked_sequence` (persisted) for resume.
Messages between `last_acked_sequence` and `last_processed_sequence` may
be replayed — the client deduplicates them.

## Ordering Guarantees

- Messages within a recipient queue are strictly ordered by sequence number
- P never reorders messages
- Gaps in sequence numbers indicate messages were deleted (TTL or overflow)
- The client can detect gaps and handle accordingly (log warning, ignore)

## Failure Scenarios

### Client crashes after processing, before acking

Messages replayed on reconnect. Client deduplicates. No data loss.

### P crashes (in-memory buffer)

Buffer lost. Sequence numbers reset. Client's persisted `last_acked_sequence`
refers to old sequence numbers. P has no messages to replay.
Client gets an empty stream. No harm — messages were lost at P, not at client.
(Future: persistent buffer eliminates this.)

### Network partition during ack

P doesn't receive the ack. Messages stay in buffer. Client reconnects,
P replays unacked messages. Client deduplicates. No data loss.

### Multiple clients for same DID

Each client independently tracks its own `last_acked_sequence`.
P uses the minimum across all clients to determine safe deletion.
(Simplification for v1: assume one client per DID, delete on any ack.)

## Implementation Plan

### Phase 1: Ack endpoint on P

Add `POST /ack/{recipient_did}` to the intermediary.
On receive: delete messages with sequence <= `up_to_sequence` from buffer.
Log the ack for audit.

### Phase 2: Client resume with sequence

Pass `since_sequence` as `Last-Event-ID` on initial SSE GET.
P's SSE handler already supports this (replays from given ID).
Client reads persisted cursor on startup.

### Phase 3: Client ack sending

After processing messages, client sends ack POST to P.
Client persists `last_acked_sequence` locally.

### Phase 4: Client dedup

Track `last_processed_sequence` in the SSE stream (already implemented).
Skip messages with sequence <= last_processed.

### Phase 5: TEAgent integration

- tspchat: persist cursor, send acks, resume on start
- gateway: same, shared cursor file per agent

## Wire Format

### Ack Request

```
POST /ack/{recipient_did}
Content-Type: application/json

{
  "up_to_sequence": 42
}

Response: 200 OK
```

### Resume (via SSE)

```
GET /messages/{recipient_did}
Accept: text/event-stream
Last-Event-ID: 42

Response: 200 OK
Content-Type: text/event-stream

id: 43
data: <message>

id: 44
data: <message>

:keepalive
```

### Message Delivery (SSE event)

```
id: <sequence_number>
data: <CESR-T encoded TSP message>
```

## Configuration

### Intermediary (P)

```
--buffer-ttl 604800      # 7 days in seconds
--buffer-max 2000        # Max messages per recipient
--ack-endpoint true      # Enable ack endpoint (default: true)
```

### Client

```
cursor_path: ~/.teagent/<agent>/buffer_cursor
ack_interval: immediate  # or batched with delay
```
