# DragonflyDB Integration Plan

> **Role:** DragonflyDB is a **caching, session, rate-limiting, and real-time pub/sub layer** alongside MongoDB. MongoDB remains the system of record. DragonflyDB is volatile/in-memory and holds only derived/cached state that can be fully reconstructed from MongoDB.

## What DragonflyDB is

DragonflyDB is a Redis-compatible in-memory datastore with a multi-threaded shared-nothing architecture. It supports the full Redis command surface (strings, hashes, lists, sorted sets, sets, streams, pub/sub, Lua scripting, TTL, ACLs) at 25× Redis throughput on a single instance. It is **not** a document database — it has no BSON, no aggregation pipeline, no `$jsonSchema` validators, no multi-document ACID transactions, and no field-level encryption.

The full compatibility matrix is at <https://www.dragonflydb.io/docs/command-reference/compatibility>.

## Architecture

```text
┌──────────────┐     ┌──────────────────────────┐     ┌─────────────────┐
│   Browser     │◄───►│    game-server (Rust)     │◄───►│   MongoDB        │
│  (SSR/WASM)  │     │                           │     │  (replica set)   │
└──────────────┘     │  ┌─────────────────────┐  │     │  System of record│
                     │  │  Repository facade   │  │     └─────────────────┘
                     │  │  (Mongo-backed)      │  │
                     │  └─────────────────────┘  │
                     │  ┌─────────────────────┐  │     ┌─────────────────┐
                     │  │  CacheService        │◄─┼────►│  DragonflyDB     │
                     │  │  (Dragonfly-backed)   │  │     │  (in-memory)    │
                     │  └─────────────────────┘  │     └─────────────────┘
                     └──────────────────────────┘
```

**Data flow rule:** every read from DragonflyDB can fall through to MongoDB on miss. Every write to MongoDB invalidates or refreshes the corresponding DragonflyDB key. No authoritative state lives only in DragonflyDB.

## SDK and crate selection

- **DragonflyDB SDK:** [`redis-rs`](https://github.com/redis-rs/redis-rs), published on crates.io as the [`redis`](https://crates.io/crates/redis) crate. DragonflyDB lists `redis-rs` as its officially supported Rust SDK in the [DragonflyDB SDK documentation](https://www.dragonflydb.io/docs/development/sdks).
- `redis = "1.4.1"` — use this `redis-rs` crate for the async DragonflyDB protocol client. DragonflyDB is Redis wire-protocol compatible, so no Dragonfly-specific Rust client is required.
- `deadpool-redis = "0.23.0"` — optional Tokio connection pooling around `redis-rs`, with managed connections, health checks, and bounded pool sizes.

Use `redis-rs` as the direct application SDK and treat `deadpool-redis` only as its pool adapter. Pin compatible versions in `Cargo.lock` and verify the selected features against the project's Rust/Tokio version during implementation.

## Use cases

### 1. Session token lookup (hot path)

**Problem:** every authenticated request resolves the bearer token to an account session. This is a single-key lookup by `token_digest` and is the hottest read path in the application.

**Design:**

```text
Key:    session:{token_digest}
Value:  serialized SessionCacheEntry (JSON or MessagePack)
TTL:    min(session.idle_expires_at, 300s) — never exceeds MongoDB idle expiry
```

`SessionCacheEntry` contains only what the request middleware needs:

```rust
struct SessionCacheEntry {
    account_id: String,
    role: String,               // "admin" | "user"
    session_id: String,
    idle_expires_at: DateTime<Utc>,
    absolute_expires_at: DateTime<Utc>,
    password_role_version: u32, // invalidated on password/role change
}
```

**Flow:**

1. Middleware receives bearer token, computes `SHA-256(raw_token)`.
2. `GET session:{digest}` from DragonflyDB.
3. On hit: validate `idle_expires_at`, `absolute_expires_at`, and `password_role_version`. If valid, proceed. If expired or version mismatched, `DEL` the key and fall through to MongoDB.
4. On miss: query MongoDB `account_sessions` by `token_digest`. If valid, populate DragonflyDB with a short TTL. If invalid, reject.
5. On logout/revocation: `DEL session:{digest}` from DragonflyDB in addition to MongoDB update.
6. On password change / role change: increment `password_role_version` in MongoDB, then `SCAN` + batch `DEL` all `session:*` keys for that account (or use a version-keyed namespace).

**Critical invariant:** DragonflyDB is a read-through cache only. Session creation, revocation, and expiry enforcement remain in MongoDB. The cache TTL is always shorter than or equal to the MongoDB idle expiry so a stale cache entry can grant at most the remaining MongoDB lifetime — which is rechecked on every request.

### 2. Auth throttle buckets (rate limiting)

**Problem:** login and sign-up attempts need per-identity rate limiting using `INCR` + `EXPIRE` atomic patterns. MongoDB requires a read-modify-write for the same effect.

**Design:**

```text
Key:    throttle:{action_kind}:{key_digest}
Value:  integer counter
TTL:    window duration (e.g. 60s, 3600s)
```

**Flow:**

1. Before expensive Argon2id verification, `INCR throttle:{action_kind}:{key_digest}`.
2. If result is 1, set TTL: `EXPIRE throttle:{action_kind}:{key_digest} {window}`.
3. If counter exceeds threshold, reject with 429 before touching Argon2id.
4. On successful auth, optionally `DEL` the short-window key to reset.

**Why DragonflyDB:** `INCR` + `EXPIRE` is a single atomic operation. No read-modify-write gap. No transaction needed. Throughput is millions of QPS.

**Fallback:** if DragonflyDB is unavailable, fall through to the MongoDB `auth_throttle_buckets` collection (which already exists in the target schema). The MongoDB path is slower but correct.

### 3. Real-time lobby and turn pub/sub

**Problem:** players in a campaign lobby or active play session need real-time updates when other players join, ready up, take turns, or when the GM advances the scene. Polling MongoDB is wasteful and high-latency.

**Design:**

```text
Channel: campaign:{campaign_id}:events
Message: JSON CampaignEvent notification
```

`CampaignEvent` types:

```rust
enum CampaignEvent {
    MemberJoined { account_id: String, display_name: String },
    MemberLeft { account_id: String },
    ReadinessChanged { account_id: String, ready: bool },
    PlaySessionStarted { play_session_id: String, mode: PlayMode },
    PlaySessionEnded { play_session_id: String },
    TurnAdvanced { play_session_id: String, actor: String, sequence: u64 },
    EncounterStarted { encounter_id: String },
    EncounterEnded { encounter_id: String, outcome: String },
    BdeChanged { account_id: String, balance: i32 },
    EventTriggered { campaign_event_id: String },
    GenerationProgress { job_id: String, state: String },
}
```

**Flow:**

1. WebSocket/SSE connection subscribes: `SUBSCRIBE campaign:{campaign_id}:events`.
2. Server publishes after every MongoDB transaction commit: `PUBLISH campaign:{campaign_id}:events {json}`.
3. Publication happens **after** the MongoDB transaction commits, never before. If the transaction rolls back, no pub/sub message is sent.
4. On disconnect/reconnect, the client requests the current state from MongoDB and resubscribes.

**Why DragonflyDB:** native pub/sub with millions of QPS. No polling. Subscribers receive notifications in real time.

### 4. Generation job work queue (lightweight)

**Problem:** the generation worker needs to claim text/image jobs. MongoDB `findOneAndUpdate` works, but a Redis-style `BLPOP` queue is lower-latency for high-volume short jobs.

**Design (optional, only if generation throughput warrants it):**

```text
List key: queue:generation:{purpose}    (e.g. queue:generation:image, queue:generation:text)
Element: JSON { job_id, priority, available_at }
```

**Flow:**

1. Enqueue: MongoDB `generation_jobs` insert (authoritative) + `RPUSH queue:generation:{purpose} {job_id}` (notification).
2. Worker: `BLPOP queue:generation:{purpose} {timeout}` to get `job_id`, then claim via MongoDB `findOneAndUpdate` (authoritative lease).
3. If MongoDB claim fails (already claimed or cancelled), the worker discards the `job_id` and loops.
4. Stale lease recovery remains a MongoDB query.

**Why DragonflyDB:** `BLPOP` blocking pop is O(1) and non-polling. Workers wake instantly when a job is available.

**When to use:** only if the polling interval on MongoDB's `findOneAndUpdate` approach becomes a bottleneck. For the initial launch with a single Orange Pi deployment, MongoDB-only job claiming is likely sufficient.

**When NOT to use:** do not make DragonflyDB the authoritative queue. If it loses data (OOM, restart, flush), the MongoDB `generation_jobs` collection is the recovery source. Workers must handle a DragonflyDB queue miss gracefully by falling back to MongoDB polling.

## Connection and configuration

### Environment variables

```bash
# MongoDB (system of record)
MONGODB_URI=mongodb://user:pass@localhost:27017/manchester_dnd?replicaSet=rs0
MONGODB_DATABASE=manchester_dnd

# DragonflyDB (cache/pub-sub layer)
DRAGONFLY_URL=redis://:password@localhost:6379/0
DRAGONFLY_POOL_SIZE=8
DRAGONFLY_TIMEOUT_MS=2000

# Feature flags
DRAGONFLY_ENABLED=true   # if false, all cache operations are no-ops; all pub/sub is skipped
```

### Coolify / Docker Compose

```yaml
services:
  dragonfly:
    image: docker.dragonflydb.io/dragonflydb/dragonfly:latest
    command: dragonfly --requirepass=${DRAGONFLY_PASSWORD}
    ports:
      - "127.0.0.1:6379:6379"
    volumes:
      - dragonfly-data:/data
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "redis-cli", "-a", "${DRAGONFLY_PASSWORD}", "ping"]
      interval: 10s
      timeout: 5s
      retries: 5

volumes:
  dragonfly-data:
```

DragonflyDB persists to disk via RDB/AOF snapshots by default. However, treat this persistence as best-effort — the application must be fully correct with a completely empty DragonflyDB instance.

### Graceful degradation

The application must handle DragonflyDB unavailability without failing:

```rust
// CacheService pattern
async fn get_session(&self, token_digest: &str) -> Option<SessionCacheEntry> {
    if !self.enabled {
        return None; // fall through to MongoDB
    }
    match self.pool.get().await {
        Ok(mut conn) => conn.get(format!("session:{}", token_digest)).await
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice(&v).ok()),
        Err(_) => None, // pool error; fall through to MongoDB
    }
}
```

Every DragonflyDB operation is wrapped so that errors produce a cache miss (read path) or a no-op (write path), never a user-facing failure.

## Rust module structure

```text
crates/game-server/src/
  cache/
    mod.rs              -- CacheService facade, graceful degradation
    session.rs          -- session token cache (GET/SET/DEL)
    throttle.rs         -- rate-limit INCR/EXPIRE
    pubsub.rs           -- campaign event publisher + subscriber helpers
    queue.rs            -- optional generation job queue (BLPOP/RPUSH)
  config.rs             -- DragonflyConfig struct
  context.rs            -- ServerContext holds CacheService alongside Repository
```

`CacheService` is injected into `ServerContext` alongside `Repository`. Application services call `CacheService` for reads and `Repository` for authoritative writes. After a successful write, the service calls `CacheService` to invalidate or refresh the affected keys.

## Security

1. **DragonflyDB requires authentication** (`--requirepass`). Never expose it unauthenticated.
2. **Bind to loopback** in development and behind a private network in production.
3. **No plaintext secrets in DragonflyDB.** Session cache entries contain `account_id` and `session_id` (which are not secrets) but never raw tokens, passwords, email, or encryption keys. The lookup key is `SHA-256(raw_token)`, not the raw token.
4. **No PII in DragonflyDB.** Session cache entries contain opaque IDs and timestamps only. No email, username, display name, or character data.
5. **ACLs:** create a dedicated DragonflyDB user with restricted command access if the deployment supports it. The application needs only `GET`, `SET`, `DEL`, `INCR`, `EXPIRE`, `PUBLISH`, `SUBSCRIBE`, `BLPOP`, `RPUSH`, `SCAN`, `PING`.
6. **TTL on everything.** No DragonflyDB key should persist indefinitely. If a key has no natural TTL, set a maximum staleness bound (e.g. 300s for session cache, 3600s for throttle buckets).
7. **Flush safety.** `FLUSHDB` or `FLUSHALL` on DragonflyDB must not cause data loss. The application must verify this by testing with an empty DragonflyDB.

## Backup and recovery

- **DragonflyDB does not need to be backed up.** All state is derivable from MongoDB.
- On restart with an empty DragonflyDB, the application experiences cache misses and repopulates naturally. The first few requests after a restart will be slower (MongoDB fallback) but correct.
- Do not include DragonflyDB in the MongoDB backup/restore verification gate. The restore test proves MongoDB + object storage + keys; DragonflyDB is disposable.

## Testing

### Unit tests

- `CacheService` with a mock Redis/Dragonfly connection.
- Graceful degradation when `DRAGONFLY_ENABLED=false` or pool errors occur.
- Session cache entry serialization/deserialization.
- Throttle counter increment and threshold logic.
- Pub/sub message serialization.

### Integration tests

- Against a real DragonflyDB instance (Docker container in CI).
- Session cache hit/miss/fallthrough cycle.
- Throttle bucket increment + expiry.
- Pub/sub publish + subscribe round-trip.
- `FLUSHDB` simulation: verify application correctness with empty cache.
- Concurrent session creation: verify no cache stampede (single MongoDB read populates cache).

### Integration with MongoDB tests

- After every MongoDB transaction commit that mutates cached data, verify DragonflyDB is invalidated/refreshed.
- After `DEL session:{digest}`, verify the next request falls through to MongoDB.
- After session revocation in MongoDB, verify the cache entry is deleted.

## Implementation phasing

DragonflyDB integration is **not on the critical path** of the MongoDB rewrite. It should be added after the core MongoDB persistence layer is working and tested. Recommended phasing:

| Phase | When | What |
|---|---|---|
| Phase 3 (Auth) | After MongoDB session store works | Add `CacheService` skeleton, session cache, throttle buckets |
| Phase 5 (Campaigns) | After play sessions work | Add pub/sub for lobby/turn events |
| Phase 8 (Generation) | Only if needed | Add optional generation job queue |
| Phase 10 (Operations) | Final | Add DragonflyDB health check to startup, monitoring, and graceful degradation tests |

## Decision: is DragonflyDB needed for launch?

**No.** The application is fully correct without DragonflyDB. Every DragonflyDB use case has a MongoDB-only fallback that is slower but correct.

**Recommended:** add DragonflyDB after Phase 3 (auth) is working, starting with session caching and throttle buckets. Measure the latency improvement. Add pub/sub when the lobby/play-session UI needs real-time updates. Skip the generation job queue unless polling becomes a measured bottleneck.

## Coolify-specific notes

- DragonflyDB is available as a Coolify service template. Deploy it as a separate service in the same project.
- Use Coolify's internal networking to connect the application container to DragonflyDB without exposing the Redis port externally.
- Set `DRAGONFLY_URL` as a Coolify environment variable pointing to the internal service name (e.g. `redis://:password@dragonfly:6379/0`).
- DragonflyDB's memory usage should be bounded. On an Orange Pi with limited RAM, set `--maxmemory` and `--maxmemory-policy=allkeys-lru` to prevent OOM. The LRU eviction is safe because all cached data is reconstructable from MongoDB.
- Monitor DragonflyDB memory, connected clients, and command latency via Coolify's built-in monitoring or a `redis-cli INFO` health check.
