# Message Buffering and Delivery Protocol Survey

Research survey of how major messaging services implement message buffering, delivery
tracking, and offline message retrieval. Conducted April 2026.

---

## 1. Signal

### Message ID Assignment
- **Server-assigned**: The server generates a `serverGuid` (UUID string) and
  `serverTimestamp` (uint64) for each envelope. The client also provides its own
  `timestamp` field (sender timestamp). Internally, a monotonic `messageId` counter is
  incremented per-device queue in Redis via ZADD.
- The protobuf `Envelope` contains: `type`, `sourceUuid`, `sourceDevice`, `timestamp`
  (sender), `serverGuid`, `serverTimestamp`, and the encrypted `content`.

### Buffer Management
- **Two-tier storage**: Messages land first in Redis (MessagesCache) for fast real-time
  delivery. A `MessagePersister` worker asynchronously migrates undelivered messages to
  DynamoDB.
- **TTL-based deletion**: DynamoDB entries have a 7-day TTL. Encrypted media on Signal
  servers is retained for up to 45 days.
- **Per-device queues**: Each device (phone, each linked device) has its own independent
  ephemeral queue. A PostgreSQL-era rule kept a maximum of 1000 messages per destination;
  the DynamoDB version relies on 7-day TTL instead.
- Deletion trigger: Message is removed from queue once the client acknowledges receipt
  (ack-based), OR the TTL expires (TTL-based). Both mechanisms apply.

### Client Resume Protocol
- Client maintains a persistent WebSocket connection to the server.
- On reconnect, the client opens a new WebSocket. The server immediately begins draining
  the per-device queue (Redis first, then DynamoDB) and pushes stored messages over the
  WebSocket.
- There is no explicit "since" token. The server simply delivers everything remaining in
  the device queue. The client does not need to specify a resume point.

### Acknowledgment
- **Per-message**: After the client receives and processes a message delivered over
  WebSocket, it sends an acknowledgment back. The server then removes that specific
  message from the queue.
- If offline, a push notification (via FCM/APNs) wakes the client, which then opens a
  WebSocket to drain the queue.

### Ordering Guarantees
- Messages within a single device queue are ordered by `messageId` (monotonic counter).
- Cross-device ordering is not guaranteed; resent messages carry the original sender
  timestamp, so they may appear out of chronological order in the UI.

### Delivery Semantics
- **At-least-once**: Messages are retained until acknowledged. If the ack is lost, the
  message may be redelivered. The client is expected to deduplicate using `serverGuid`.

### Pagination
- Not paginated in the traditional sense. The server streams all queued messages over the
  WebSocket connection. The queue is drained incrementally as the client acks each message.

---

## 2. WhatsApp

### Message ID Assignment
- **Server-assigned**: WhatsApp servers generate a unique message ID. The same ID is
  referenced in all delivery reports for that message. Timestamps are server-assigned in
  epoch format; the server overrides any client-side clock manipulation.

### Buffer Management
- Built on a customized Erlang/Ejabberd stack using Mnesia as the in-memory database.
- Offline messages are stored in Mnesia tables, replicated across multiple servers for
  fault tolerance.
- Messages remain queued until the recipient device connects and retrieves them. Exact
  retention limits are not publicly documented, but the system is designed for relatively
  short offline windows (days, not weeks).

### Client Resume Protocol
- WhatsApp uses a customized XMPP-derived protocol ("FunXMPP") over a persistent TCP/TLS
  connection.
- On reconnect, the client re-establishes the connection. The server pushes all queued
  offline messages from Mnesia to the client.
- Like Signal, there is no explicit "give me messages since X" mechanism for the primary
  flow; the server pushes the entire offline queue.

### Acknowledgment
- **Per-message with multi-stage receipts**:
  - Single grey tick: message sent to server.
  - Double grey ticks: message delivered to recipient device (server received FunXMPP ack
    from recipient).
  - Double blue ticks: message read by recipient (recipient client sent read receipt).
- The server deletes the queued message after receiving the delivery ack from the
  recipient client.

### Ordering Guarantees
- Messages are ordered by server timestamp within a conversation. The Ejabberd routing
  layer processes messages sequentially per-recipient.

### Delivery Semantics
- **At-least-once**: Messages are stored until delivery ack is received. Duplicates are
  possible if ack is lost.

### Pagination
- Not publicly documented. The offline queue is pushed to the client upon reconnection.
  For message history (beyond the offline queue), WhatsApp historically relied on
  client-side local storage, not server-side history.

---

## 3. Matrix

### Message ID Assignment
- **Server-assigned**: The homeserver assigns an `event_id` to each event (message).
  - Room versions 1-2: format `$randomstring:example.org`
  - Room versions 3+: format is a URL-safe base64 hash like
    `$acR1l0raoZnm60CBwAVgqbZqoO/mYU81xysh1u7XcJk`
- The client provides a `txnId` (transaction ID) when sending, used for idempotency.
  The server maps this to the authoritative `event_id`.

### Buffer Management
- **Persistent event storage**: Matrix homeservers store all events permanently (by
  default). Events are part of the room's DAG (directed acyclic graph) and are never
  deleted from the server unless explicitly redacted.
- There is no TTL-based deletion. The server retains the full room history. Server admins
  can configure purge policies, but the protocol assumes full retention.

### Client Resume Protocol
- **Sync API** (`/sync` endpoint):
  - The client calls `GET /sync` with an optional `since` parameter containing a
    `next_batch` token from the previous sync response.
  - First call (no `since`): full initial sync of all joined rooms.
  - Subsequent calls: incremental sync returning only new events since the token.
  - The server returns a `next_batch` token in every response for the next request.
- **Sliding Sync** (Matrix 2.0, MSC3575/MSC4186):
  - Uses a `pos` token instead of `since`. This token is ephemeral and can be
    invalidated, causing graceful fallback to initial sync.
  - More efficient: only syncs rooms the client is actively viewing.
- **Messages API** (`/messages` endpoint):
  - Used for paginating room history (scrollback).
  - Takes `from` and `to` stream tokens, `dir` (direction), and `limit`.
  - Tokens are opaque strings matching `[a-zA-Z0-9.=_-]+`.

### Acknowledgment
- **Read receipts** (`m.read`): Client sends a read receipt event for the last event it
  has read. This is not an ack for delivery but for display.
- **No delivery ack in the protocol**: The sync API itself acts as the implicit ack. Once
  the client has received a sync response with certain events, the server knows they were
  delivered (the client will use the new `next_batch` token going forward).
- If the client crashes before processing, it re-syncs from the last `next_batch` token
  it persisted, getting the same events again. Client-side deduplication by `event_id`.

### Ordering Guarantees
- Events within a room are topologically ordered in the room DAG. The server linearizes
  this into a stream order for the sync response.
- Gaps can occur in federated scenarios. The server resolves state and fills gaps via
  federation backfill.

### Delivery Semantics
- **At-least-once**: If a client re-syncs from an old token, it will receive events
  again. Deduplication is by `event_id` on the client.

### Pagination
- **Fully paginated**: The sync response can include a `limited: true` flag in a room's
  timeline, indicating there are more events than returned. The client uses the
  `prev_batch` token with the `/messages` endpoint to paginate backward.
- `/messages` responses include `start` and `end` tokens, plus a `chunk` array of events.
  A `complete: true` or absence of an `end` token signals no more pages.

---

## 4. XMPP

### Message ID Assignment
- **Client-assigned** (stanza `id` attribute): The sender sets a unique `id` on each
  `<message/>` stanza. This is typically a UUID generated by the client.
- **Server-assigned archive ID** (MAM, XEP-0313): When the server archives a message, it
  assigns its own archive `id` (also called `uid`). This is an opaque string, often
  monotonically increasing, used for pagination in MAM queries.

### Buffer Management
- **Offline messages** (XEP-0160): The server stores messages destined for offline bare
  JIDs. Messages are delivered on the next login and then deleted.
- **Message Archive Management** (XEP-0313, MAM): The server maintains a persistent
  message archive. Messages are stored indefinitely (configurable by server admin). This
  is separate from the offline queue -- MAM provides full history access.

### Client Resume Protocol
- **Stream Management** (XEP-0198): Allows resuming a stream after disconnect.
  - On enabling stream management, both sides maintain a stanza counter `h`.
  - On resume, the client sends `<resume h='N' previd='stream-id'/>` where `h` is the
    count of stanzas the client has handled, and `previd` is the session identifier.
  - The server replays any stanzas the client has not acked (those with sequence > N).
  - Resume window is server-configurable (typically seconds to minutes).
- **MAM catchup**: For longer offline periods, the client queries MAM with a `start`
  timestamp or the last known archive `id`, and the server returns all matching messages.
  The query uses RSM (Result Set Management, XEP-0059) for pagination.

### Acknowledgment
- **XEP-0198 (Stream Management)**: The server sends `<r/>` (request ack). The client
  replies `<a h='N'/>` where N = count of stanzas handled. This is a cumulative ack
  (batched), not per-message.
  - Typically requested every 5 stanzas for efficiency.
- **XEP-0184 (Message Delivery Receipts)**: End-to-end delivery receipts. The recipient
  sends a `<received/>` stanza referencing the original message `id`. This is per-message.

### Ordering Guarantees
- XMPP stanza order is preserved within a single stream (TCP guarantees ordering).
- MAM results are ordered by server archive ID (monotonically increasing).
- MAM `<fin complete='true'/>` tells client whether there are more pages.
- A `stable='false'` attribute on `<fin/>` warns of potentially inconsistent results
  (network partitions, etc.).

### Delivery Semantics
- **At-least-once**: XEP-0198 allows unacked stanzas to be replayed on resume. Client
  must deduplicate. MAM queries can also return already-seen messages.

### Pagination
- **RSM (XEP-0059)**: MAM queries return results in pages.
  - The `<set/>` element in the response contains `<first/>`, `<last/>`, and `<count/>`.
  - The client uses `<after>last-id</after>` or `<before>first-id</before>` for next/prev
    page.
  - The `<fin complete='true'/>` attribute indicates the final page.
  - MAM archive IDs can be used directly in the initial query (not just after first
    result).

---

## 5. MQTT

### Message ID Assignment
- **Client-assigned packet identifier**: A 16-bit integer (1-65535) assigned by the
  sender for QoS 1 and QoS 2 messages. This is NOT a globally unique message ID; it is a
  session-scoped flow control identifier reused after acknowledgment.
- The broker does not assign a persistent message ID. The packet identifier is only
  meaningful within the current session and QoS handshake flow.
- **MQTT v5 additions**: Correlation Data and Response Topic properties can carry
  application-level identifiers, but these are not part of the core QoS mechanism.

### Buffer Management
- **Persistent sessions** (MQTT 3.1.1: `CleanSession=0`; MQTT 5: `CleanStart=0`):
  - Broker stores: subscriptions, unacknowledged QoS 1/2 messages, and new QoS 1/2
    messages published to subscribed topics while client is offline.
  - Messages are queued until the client reconnects and acknowledges them.
- **MQTT v5 Session Expiry Interval**: Specifies how long (in seconds) the broker retains
  the session after disconnect. Range: 0 to 0xFFFFFFFF (never expire). Default: 0 (delete
  on disconnect, same as CleanSession=1).
- **MQTT v5 Message Expiry Interval**: Per-message TTL in seconds. Broker discards
  message if TTL expires before delivery. Broker decrements the interval and includes the
  remaining TTL when delivering.
- Broker-specific limits apply (e.g., max queued messages, queue memory limits).

### Client Resume Protocol
- Client reconnects with `CleanStart=0` (MQTT v5) or `CleanSession=0` (MQTT 3.1.1) and
  the same Client ID.
- Broker checks for existing session state. If found, it resumes: delivers queued messages
  and retransmits any unacknowledged QoS 1/2 messages.
- The client does NOT send a "since" token. Session state is matched by Client ID.

### Acknowledgment
- **QoS 0**: No ack. Fire-and-forget.
- **QoS 1**: Sender publishes PUBLISH. Receiver sends PUBACK with matching Packet ID.
  Per-message ack.
- **QoS 2**: Four-step handshake:
  1. Sender -> PUBLISH (with Packet ID)
  2. Receiver -> PUBREC (ack receipt, stores message)
  3. Sender -> PUBREL (release, can free Packet ID after PUBCOMP)
  4. Receiver -> PUBCOMP (complete, can delete stored reference)
  - The PUBREL acts as a boundary: any PUBLISH with the same Packet ID arriving before
    PUBREL is a duplicate; after PUBREL it is a new message.

### Ordering Guarantees
- **Per-topic, per-QoS level**: MQTT 3.1.1 Section 4.6 specifies that the order of
  QoS 1 and 2 messages is guaranteed within a single topic subscription, provided
  `Receive Maximum` is 1 (no concurrent in-flight messages).
- With multiple in-flight messages, ordering is not guaranteed because retransmissions
  can interleave.
- QoS 0 has no ordering guarantees beyond TCP ordering.

### Delivery Semantics
- **QoS 0**: At-most-once.
- **QoS 1**: At-least-once (PUBACK may be lost, causing retransmission).
- **QoS 2**: Exactly-once (four-step handshake ensures no duplicates).

### Pagination
- Not applicable. MQTT is a real-time pub/sub protocol with no concept of message history
  or pagination. The broker delivers queued messages as fast as the client can consume
  them. There is no history query mechanism in the protocol.

---

## 6. Apple APNs / Google FCM

### Message ID Assignment
- **APNs**:
  - Server (provider) can set `apns-id` header (UUID format). If omitted, APNs generates
    one. This ID is used in error responses to identify which notification failed.
  - `apns-collapse-id` header (up to 64 bytes) controls coalescing. Multiple notifications
    with the same collapse-id replace each other.
- **FCM**:
  - Server-assigned: FCM returns a `message_id` (string) in the send response to confirm
    acceptance.
  - Provider sets `collapse_key` (FCM legacy) or `collapseKey` (HTTP v1) for notification
    coalescing. Up to 4 different collapse keys per device can be stored simultaneously.

### Buffer Management
- **APNs**:
  - Stores **one notification per app per device** when offline. Each new notification
    replaces the previous one (unless different `apns-collapse-id` values are used, but
    even then the storage is minimal).
  - Retention: up to the `apns-expiration` header value (Unix epoch timestamp). Maximum
    ~28 days. If set to 0, no storage (immediate-or-drop).
- **FCM**:
  - Stores up to **100 pending messages per app per device**.
  - Default TTL: 4 weeks (2,419,200 seconds). Configurable from 0 to 2,419,200 seconds.
  - Collapsible messages: only the latest per collapse key is stored (max 4 collapse keys
    concurrently).
  - Non-collapsible messages: all stored up to the 100-message limit.

### Client Resume Protocol
- **No explicit resume mechanism from the app's perspective**. Push notification services
  are transparent to the receiving app:
  - The OS manages the connection to APNs/FCM.
  - When the device comes online, the OS reconnects and the service delivers stored
    notifications.
  - The app receives notifications via OS callbacks; it does not "request" missed
    notifications.

### Acknowledgment
- **APNs**: No application-level ack from device to APNs for received notifications. The
  OS-level connection handles transport ack. The provider receives synchronous HTTP/2
  responses (success/failure) when sending.
- **FCM**: Similar. No client-to-FCM ack for received messages. The provider gets a
  response with `message_id` or error on send. For data messages, the app can implement
  its own ack logic back to the app server.

### Ordering Guarantees
- **No ordering guarantees**. Push notifications are best-effort and may arrive out of
  order. Multiple notifications sent in rapid succession may be coalesced or reordered.

### Delivery Semantics
- **At-most-once** (with coalescing): APNs keeps only the latest notification per app.
  Earlier ones are lost. FCM keeps up to 100 but with TTL expiry.
- For collapsible notifications, only the latest is delivered (earlier versions are
  discarded).

### Pagination
- Not applicable. Stored notifications are delivered as a batch when the device comes
  online. There is no pagination or query mechanism.

---

## Comparison Table

| Dimension | Signal | WhatsApp | Matrix | XMPP | MQTT | APNs/FCM |
|---|---|---|---|---|---|---|
| **ID Assignment** | Server (serverGuid UUID + monotonic counter) | Server (unique msg ID, epoch timestamp) | Server (event_id, hash-based in v3+) | Client (stanza id) + Server (MAM archive id) | Client (16-bit packet ID, session-scoped) | Provider or server (UUID / message_id) |
| **Buffer Storage** | Redis + DynamoDB | Mnesia (in-memory, replicated) | Persistent event store (DAG) | Offline store + MAM archive | Broker session store | APNs: 1 per app; FCM: up to 100 |
| **Retention** | 7-day TTL (DynamoDB); 45 days for media | Until delivery (short-term) | Indefinite (full history) | MAM: indefinite (configurable). Offline: until login | Session Expiry Interval (MQTT v5); broker-specific | APNs: up to 28 days; FCM: up to 28 days |
| **Deletion Trigger** | Ack + TTL | Delivery ack | Never (redaction only) | Offline: delivery. MAM: admin policy | Ack + Session Expiry + Message Expiry | TTL expiry or replacement by newer notification |
| **Resume Mechanism** | Reconnect WebSocket; server drains queue | Reconnect TCP; server pushes queue | `since` token on `/sync`; `pos` on Sliding Sync | XEP-0198 `<resume h='N'>` + MAM query with last archive ID | Reconnect with same Client ID + CleanStart=0 | OS reconnects automatically; transparent to app |
| **Ack Type** | Per-message (WebSocket ack) | Per-message (FunXMPP ack + read receipts) | Implicit (sync token advance) + read receipts | Cumulative (XEP-0198 stanza count) + per-message (XEP-0184) | Per-message (PUBACK/QoS1) or 4-step (QoS2) | None (transport-level only) |
| **Ordering** | Per-device queue (monotonic counter) | Server timestamp ordered | Topological (room DAG) + stream order | Stream order (TCP) + MAM archive order | Per-topic if Receive Maximum=1 | No guarantees |
| **Delivery Semantics** | At-least-once | At-least-once | At-least-once | At-least-once | QoS 0: at-most-once; QoS 1: at-least-once; QoS 2: exactly-once | At-most-once (with coalescing) |
| **Pagination** | No (stream drain) | No (queue push) | Yes (sync limited + /messages with tokens) | Yes (MAM with RSM, page by archive ID) | No (real-time delivery) | No (batch delivery) |

---

## Common Patterns Across Services

### Pattern 1: Two-Phase ID Assignment
Most services assign IDs at two levels:
- A **client-provided ID** for idempotency and deduplication (Signal's sender timestamp,
  Matrix's txnId, XMPP's stanza id, MQTT's packet identifier).
- A **server-assigned ID** as the authoritative, globally unique identifier (Signal's
  serverGuid, Matrix's event_id, XMPP's MAM archive id, APNs' apns-id).

### Pattern 2: Per-Device/Per-Session Queuing
Signal, WhatsApp, and MQTT all maintain per-device (or per-client-session) queues. This
isolates message delivery state per endpoint and avoids the complexity of tracking which
of many devices has received which messages. Matrix takes a different approach with
per-user sync tokens, but achieves a similar effect.

### Pattern 3: Ack-Then-Delete
The dominant pattern for buffer management is: store messages until the client
acknowledges receipt, then delete. Signal, WhatsApp, XMPP offline messages, and MQTT all
follow this pattern. TTL acts as a safety net (Signal: 7 days, MQTT v5: configurable,
FCM: 28 days).

### Pattern 4: Opaque Resume Tokens vs. Queue Drain
Two distinct resume strategies emerge:
- **Token-based resume** (Matrix, XMPP MAM): Client stores an opaque token (next_batch,
  archive ID) and sends it on reconnect. Server returns everything after that token.
  This is stateless on the client side beyond the token.
- **Queue drain** (Signal, WhatsApp, MQTT): Server maintains per-client state. Client
  simply reconnects, and the server pushes everything in the queue. No client-side token
  needed.

### Pattern 5: At-Least-Once as the Default
Every messaging service (except APNs/FCM and MQTT QoS 0) defaults to at-least-once
delivery. Exactly-once is expensive (MQTT QoS 2's four-step handshake) and rarely used in
messaging. Deduplication is pushed to the client using message IDs.

### Pattern 6: Separation of Transport Ack and Semantic Receipts
All services distinguish between:
- **Transport acknowledgment**: "I received the bytes" (XEP-0198 stanza count, Signal
  WebSocket ack, MQTT PUBACK).
- **Semantic receipts**: "The user read the message" (WhatsApp blue ticks, Matrix
  m.read, XMPP XEP-0184).
Buffer deletion is keyed on transport ack, not semantic receipts.

### Pattern 7: Pagination Only for History, Not Real-Time
Only Matrix and XMPP (MAM) support true paginated history queries. The real-time delivery
path in all services is stream-based (push all pending messages). Pagination is reserved
for historical access / scrollback.

---

## Recommended Design for TSP Intermediary Buffer Protocol

Based on the patterns above, here is a recommended design for the TSP intermediary node
message buffer, drawing from the strongest aspects of each surveyed protocol.

### 1. Message ID: Server-Assigned UUID + Client Transaction ID

```
MessageEnvelope {
    server_message_id: UUID,      // Assigned by intermediary, globally unique
    server_timestamp:  u64,       // Monotonic server clock (millis since epoch)
    client_txn_id:     String,    // Client-provided, used for idempotency/dedup
    sender_vid:        String,    // TSP Verified Identifier of sender
    recipient_vid:     String,    // TSP Verified Identifier of recipient
    payload:           Bytes,     // Encrypted TSP message
}
```

**Rationale**: Follows the two-phase ID pattern (Signal, Matrix, XMPP). The
`server_message_id` is the canonical reference. `client_txn_id` allows the sender to
retry without creating duplicates (the intermediary deduplicates on `client_txn_id`
within a time window). `server_timestamp` provides monotonic ordering.

### 2. Per-Recipient Queue with Ack-Based Deletion + TTL Safety Net

- The intermediary maintains a **per-recipient-VID queue** (like Signal's per-device
  queue).
- Messages are appended with a monotonically increasing **sequence number** within the
  queue (inspired by XMPP MAM archive IDs and Signal's messageId counter).
- Messages are deleted from the queue when the recipient acknowledges receipt.
- A configurable **TTL** (default: 7 days, matching Signal) acts as a safety net for
  unacknowledged messages.
- Optional: maximum queue depth (e.g., 1000 messages) with oldest-first eviction, as
  Signal's PostgreSQL era implemented.

```
QueueEntry {
    sequence_number:    u64,       // Monotonic within this recipient queue
    server_message_id:  UUID,
    server_timestamp:   u64,
    payload:            Bytes,
    expires_at:         u64,       // server_timestamp + TTL
}
```

### 3. Client Resume: Token-Based (Hybrid Approach)

Combine the best of both worlds -- token-based resume (Matrix) with queue-drain
simplicity (Signal):

- The client maintains a **last_acknowledged_sequence** number.
- On reconnect, the client sends: `Resume { since_sequence: u64 }`
- The intermediary returns all messages with `sequence_number > since_sequence`.
- If the client has no stored sequence (first connect or state loss), it sends
  `since_sequence = 0` and receives the entire queue.
- This is simpler than Matrix's opaque tokens (the sequence number is meaningful and
  inspectable) while being more robust than pure queue-drain (the client controls the
  resume point, enabling crash recovery).

```
ResumeRequest {
    recipient_vid:       String,
    since_sequence:      u64,      // 0 for "give me everything"
    max_count:           u32,      // Pagination: max messages per response
}

ResumeResponse {
    messages:            Vec<QueueEntry>,
    has_more:            bool,     // True if more messages remain
    latest_sequence:     u64,      // Highest sequence in queue (for client bookkeeping)
}
```

### 4. Acknowledgment: Cumulative with Explicit Ack Messages

Inspired by XMPP XEP-0198's cumulative ack (more efficient than per-message):

- The client sends: `Ack { up_to_sequence: u64 }`
- This acknowledges all messages with `sequence_number <= up_to_sequence`.
- The intermediary deletes all acknowledged messages from the queue.
- The client should ack periodically (e.g., every 5 messages or every 5 seconds,
  whichever comes first) -- matching XEP-0198's recommendation.
- For real-time delivery over WebSocket, the ack can piggyback on the next request.

**Why cumulative instead of per-message**: Reduces protocol overhead (one ack covers many
messages). Signal's per-message ack is simpler but generates more traffic. XMPP's
cumulative approach is well-proven. The sequence number makes cumulative ack
straightforward.

### 5. Ordering: Guaranteed Per-Recipient Queue

- Messages within a recipient queue are strictly ordered by `sequence_number`.
- The intermediary never reorders messages within a queue.
- Cross-sender ordering is by `server_timestamp` (the intermediary's clock), not the
  sender's clock.
- Gap detection: if the client notices a gap in sequence numbers, it knows messages were
  lost (TTL expiry) and can handle accordingly.

### 6. Delivery Semantics: At-Least-Once with Client Dedup

- **At-least-once** as the default (matching the industry consensus).
- The client deduplicates using `server_message_id` (UUID).
- Exactly-once is not pursued at the protocol level (too expensive, as MQTT QoS 2
  demonstrates). Application-level idempotency is the recommended approach.

### 7. Pagination: Bounded Response with Continuation

- Resume responses are paginated with `max_count` (client-specified batch size).
- Response includes `has_more` flag (inspired by Matrix's `limited` flag and XMPP MAM's
  `complete` attribute).
- The client iterates by advancing `since_sequence` to the highest sequence received.
- This handles the case of thousands of queued messages gracefully, without requiring
  the intermediary to stream everything at once.

### 8. Push Notification Integration

- When a message arrives for an offline recipient, the intermediary can fire a push
  notification (via APNs/FCM) as a **wake-up signal only** (following Signal's pattern).
- The push notification carries minimal metadata (no message content) -- just enough to
  prompt the client to connect and resume.
- This keeps the intermediary's buffer as the source of truth, not the push service.

### Summary of Design Choices

| Dimension | Chosen Approach | Inspired By |
|---|---|---|
| Message ID | Server-assigned UUID + client txn ID | Signal, Matrix |
| Queue model | Per-recipient-VID, monotonic sequence | Signal, MQTT |
| Retention | Ack-based deletion + configurable TTL | Signal (7d), MQTT v5 |
| Resume | Client sends `since_sequence` number | Matrix (token) + Signal (queue drain) |
| Ack | Cumulative `up_to_sequence` | XMPP XEP-0198 |
| Ordering | Strict per-queue sequence numbers | Signal, XMPP MAM |
| Delivery | At-least-once, client dedup by UUID | Industry consensus |
| Pagination | Bounded `max_count` + `has_more` | Matrix, XMPP MAM/RSM |
| Push | Wake-up only, no content | Signal |

---

## Sources

- [Signal Message Delivery Wiki](https://signal.miraheze.org/wiki/Message_delivery)
- [Signal Server API Protocol](https://github.com/signalapp/Signal-Server/wiki/API-Protocol)
- [Signal Server Source Code Analysis (SoftwareMill)](https://softwaremill.com/what-ive-learned-from-signal-server-source-code/)
- [Signal Server DeepWiki](https://deepwiki.com/signalapp/Signal-Server)
- [Signal Envelope Protobuf](https://github.com/signalapp/libsignal-service-java/blob/master/protobuf/SignalService.proto)
- [WhatsApp Architecture (GetStream)](https://getstream.io/blog/whatsapp-works/)
- [WhatsApp Architecture (CometChat)](https://www.cometchat.com/blog/whatsapps-architecture-and-system-design)
- [Matrix Client-Server API Specification](https://spec.matrix.org/latest/client-server-api/)
- [Matrix Sliding Sync MSC4186](https://github.com/matrix-org/matrix-spec-proposals/blob/erikj/sss/proposals/4186-simplified-sliding-sync.md)
- [XMPP XEP-0313: Message Archive Management](https://xmpp.org/extensions/xep-0313.html)
- [XMPP XEP-0198: Stream Management](https://xmpp.org/extensions/xep-0198.html)
- [XMPP XEP-0160: Offline Messages](https://xmpp.org/extensions/xep-0160.html)
- [MQTT v5.0 OASIS Specification](https://docs.oasis-open.org/mqtt/mqtt/v5.0/mqtt-v5.0.html)
- [MQTT QoS Explained (HiveMQ)](https://www.hivemq.com/blog/mqtt-essentials-part-6-mqtt-quality-of-service-levels/)
- [MQTT v5 Session Expiry (EMQX)](https://www.emqx.com/en/blog/mqtt5-new-feature-clean-start-and-session-expiry-interval)
- [Apple APNs Documentation](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns)
- [FCM Message Lifespan](https://firebase.google.com/docs/cloud-messaging/customize-messages/setting-message-lifespan)
- [Push Notification Delivery Internals](https://blog.clix.so/how-push-notification-delivery-works-internally/)
