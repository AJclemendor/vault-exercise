# GPT was used for formatting and light copy editing; the substance is otherwise written by me


Here are metrics for a 10 min run



- Small note as I am not very familiar with Rust: I made a point to keep non-test files under `service/src` below 500 lines.
The dedicated test files are larger, and production modules only include small `#[cfg(test)]` hooks to point at those test files. This helped me review the implementation line by line and understand the generated output.

Also, my eyesight is lowkey dying, so I tend to log some things with color and formatting because it makes runtime output easier for me to scan.


# Repo Struct

This summarizes the visible Rust service structure under `service/src`.

## Top-Level Entries

| Path | Summary |
| --- | --- |
| `engine/` | Core in-memory matching engine. It owns orders, book indexes, balance reservations, fill candidates, matching rules, and settlement state application. |
| `tasks/` | Submodules for background task implementations. Currently this holds the settlement worker logic used by `tasks.rs`. |
| `chain.rs` | Blockchain/RPC adapter. It reads token and vault balances, submits `matchOrders` transactions, confirms receipts, checks receipt status, and scans logs for users whose balances need refresh. |
| `engine_tests.rs` | Dedicated tests for engine behavior, including matching priority, balance reservation, stale order pruning, market order behavior, fill claiming, and stats accounting. |
| `main.rs` | Application entrypoint. It loads config, builds `ChainClient`, initializes shared `AppState`, starts background loops, and wires the Axum HTTP routes. |
| `routes.rs` | HTTP route handlers for order submission, cancellation, order listing, balance views, book snapshots, and stats snapshots. |
| `runtime.rs` | Runtime tuning helpers. It reads environment variables for balance cache ages, background loop intervals, RPC timeouts, receipt retries, and receipt reconciliation windows. |
| `sequencing_tests.rs` | Dedicated async tests for ordered gates and admission sequencing, including gap handling, idempotent completion, receipt apply ordering, and ticket-ordered order admission. |
| `sequencing.rs` | Ordering utilities. It provides `OrderedGate` and `AdmissionSequencer` so concurrent work can be admitted or applied in deterministic sequence order. |
| `stats.rs` | Counter and snapshot definitions for service metrics, plus percentage and ratio helpers used by the engine and `/stats` endpoint. |
| `tasks.rs` | Background task entrypoints. It runs active balance refresh, chain log polling, periodic stats logging, and re-exports the settlement loop. |
| `types.rs` | Shared API and domain types, including order side/type/status, request and response DTOs, book snapshots, balance views, and API error responses. |

## `engine/` Submodules

| Path | Summary |
| --- | --- |
| `engine/mod.rs` | Defines the main `Engine`, internal `Order` and `BalanceState` models, fill candidates, and module layout. |
| `engine/orders.rs` | Handles order admission, validation, reservation, cancellation, visible open order listing, and terminal order state transitions. |
| `engine/matching.rs` | Finds crossing orders, prepares fill candidates, manages in-flight fill state, applies successful settlements, and aborts failed fills. |
| `engine/book.rs` | Maintains limit-order price indexes and builds public book snapshots with bids, asks, spread, and midpoint. |
| `engine/balances.rs` | Manages cached balances, dirty markers, refresh freshness, user balance views, and pruning when cached balances no longer support reservations. |
| `engine/exposure.rs` | Computes hard-locked funds and checks whether users can safely keep live or in-flight orders after fills. |
| `engine/snapshot.rs` | Builds `StatsSnapshot` values and records settlement/order metric counters. |
| `engine/math.rs` | Small numeric helpers for U256 math, WAD formatting, and reservation calculations. |

## `tasks/` Submodules

| Path | Summary |
| --- | --- |
| `tasks/settlement/mod.rs` | Settlement loop coordinator. It selects sequential, receipt-concurrent, or fully concurrent settlement modes from environment config. |
| `tasks/settlement/outcome.rs` | Settlement lifecycle handling. It prechecks fills, submits transactions, confirms receipts, applies success, and reconciles uncertain outcomes. |
| `tasks/settlement/concurrency.rs` | Concurrency support for settlement workers, including user lock striping and reorder invalidation tracking. |
| `tasks/settlement/settlement_tests.rs` | Dedicated tests for settlement confirmation outcomes, uncertain receipts, unresolved fills, reverts, and send failures. |



# Verified against current app implementation
# Logic design choices 

- I honestly think I could make a strong argument for both sides of these choices so I will do my best to explain why I chose the ones I did and why I think for this the tradeoffs are worth it.


# Harness edits
The harness changes are mostly connection-pooling / runtime-control changes. They do not change the service HTTP API shape, but they do change harness-internal function signatures so shared clients can be passed through setup, provider/reader creation, order loops, and chain loops.

The upstream harness created new HTTP/RPC connections more aggressively. This branch adds shared clients so the harness can run higher concurrency without timing out from too many individual connections.

A new `HarnessClients` struct was added with two reusable clients:

- `service: reqwest::Client`
- `rpc: alloy::transports::http::reqwest::Client`

Both clients are built with a 5 second timeout and a larger idle connection pool.




# Ghost orders and what is still weak

A ghost order is an order that matches off-chain and when there is a match but the match cannot actually settle on-chain.

This can happen because the service's off-chain view of balances is only a snapshot. A user may have enough balance when we admit the order, but before settlement they can transfer tokens, withdraw from the vault, or otherwise change their on-chain state. Then when the service calls `Vault.matchOrders(...)`, the contract tries to pull funds and the transaction can revert.

The repo tries to reduce ghost orders in a few ways:

1. Admission refreshes stale balances before accepting new orders.
2. Admission waits for the request's admission turn before checking freshness, and the engine still rejects admission if the cache remains dirty or missing.
3. Users with reserved balances are refreshed in the background.
4. Chain logs mark known users dirty when transfers, matches, or withdrawals happen.
5. Settlement always does a pre-settlement refresh for both buyer and seller before submitting `Vault.matchOrders(...)`.
6. If a fill is underfunded before settlement, the service does not submit the transaction.
7. If settlement reverts, the service refreshes again, records the revert, and either releases, prunes, or stales the affected fill/orders depending on policy and post-revert funding. Users are marked dirty if the post-revert refresh fails.
8. If the service has a tx hash but cannot prove success or revert, it keeps the fill locked while rechecking. This is now bounded: after the deferred receipt checks time out, the service marks both users dirty, stales both orders, aborts the fill, and releases the reservations.

The biggest remaining weakness is that the pre-settlement refresh is not atomic with the on-chain transaction. A user can have enough balance when we check, then move funds before `Vault.matchOrders(...)` lands. I do not think this can be fully solved off-chain. The clean solution would be an on-chain reservation or escrow model, where funds are locked before orders are allowed to match. A direct socket-style connection to the user's wallet could maybe reduce the window, but it adds a lot of overhead and still seems weaker than holding funds on-platform in some way. Without that, the service can only detect the failure and handle the revert.

Other weak spots:

# Ghost Order Limitations

The service reduces ghost orders, but it does not eliminate them completely. It refreshes stale or dirty balance caches before admission, refreshes buyer/seller balances again before settlement, catches many underfunded fills before tx submission, keeps unresolved submitted fills locked at the order/reservation level while receipt checks run, and has a bounded cleanup path when the final receipt cannot be proven.

## Remaining Tradeoffs

- **Resting limit orders can overbook balance.** A user can have multiple open limit orders that are each valid alone, but cannot all settle together. This gives the book more flexibility, but it means some resting liquidity can become stale once one order fills or the user's chain balance changes.

- **Balance freshness is still a cache policy.** Live balance reads are now tied to block numbers, and dirty log events carry block ordering so an older refresh should not clear a newer dirty mark. This is safer than wall-clock-only freshness, but the service still depends on polling, cache age thresholds, and log processing latency.

- **Send failures without a tx hash are handled conservatively but are not durably tracked.** The service records a send failure, marks both users dirty, and stales both orders because it cannot prove whether the provider broadcasted the tx before failing. Production would still want durable nonce or transaction tracking around this edge case.

- **The settlement queue is in-memory and unbounded.** Under heavy load it can grow, and a process restart loses queued work plus local in-flight state.

- **There is no durable order or settlement persistence.** This is acceptable for the exercise, but production would need durable recovery for open orders, in-flight txs, user reservations, and unknown receipt outcomes.

- **The receipt timeout is bounded by design.** Bounded timeout prevents reservations from staying locked forever. The tradeoff is that if an unknown tx succeeds after the service gives up, the service releases local reservations, stales both orders, and marks both users dirty. Any later repair depends on a future admission-triggered refresh or explicit chain read; there is no durable automatic recovery.

## Production Direction

For a production version, the core improvement would be to make the off-chain view durable and provably ordered against chain state. That means persisting orders, reservations, submitted tx hashes/metadata, nonce decisions if managed by the service, receipt status, balance-read block numbers, dirty-after-block markers, and any in-memory ordering/reorder generations. A settlement worker should be able to restart, replay pending work, and prove whether a prefetched balance is still valid when its turn arrives.

The current design is a reasonable exercise-level compromise: it uses conservative pre-settlement checks, block-versioned balance refreshes, dirty marking from logs, and bounded unknown-outcome handling, but it still accepts that ghost orders can happen under adversarial chain activity.




# General concurrency things

Before these changes, the harness created a lot of concurrent pressure, but the service was doing too much slow work in the wrong places. Matching was more background-driven, market orders could wait around instead of crossing immediately, and the old book logic relied more on broad scans. Under load, that made the system slower and caused more orders to miss admission or matching opportunities.

Now, the harness reuses pooled HTTP/RPC clients, so it can generate cleaner high-concurrency load without wasting time on connection overhead. On the service side, `POST /orders` still accepts concurrent requests, but admission and matching are sequenced so the book stays deterministic. Market orders cross immediately and cancel any leftover size, limit orders cross immediately if marketable, and fill candidates are pushed to async settlement workers.

Order is maintained mainly in `service/src/routes.rs`, `service/src/sequencing.rs`, and the engine modules. Each order request gets an admission ticket, waits for its turn, then mutates the engine while holding the engine mutex. That means even if many HTTP requests arrive at once, only one request at a time can assign order IDs, insert into the book, create fills, and update reserved balances. The book itself uses sorted `BTreeMap` price levels and FIFO `VecDeque`s inside each level, so price-time priority is preserved.

The main async work lives in `service/src/tasks/settlement/`. The HTTP path creates fill candidates and pushes them onto the settlement queue, but workers handle the settlement blockchain work later. Admission can still refresh a user's balance on-chain before accepting an order. Settlement can refresh buyer/seller balances, submit `Vault.matchOrders(...)`, and wait for receipts asynchronously. In `receipt_concurrent` mode, receipt work is bounded and applied in order. In full `concurrent` mode, worker/in-flight/receipt semaphores, per-user locks, and tx/apply gates bound background work while preserving fill-order application.

The main result is that settlement blockchain work is pushed out of the request path while the matching decision happens right away. The core ordering rules did not change. The service still processes book mutations one at a time through the admission sequencer and engine mutex, so order IDs, fill IDs, price-time priority, and FIFO within each price level stay deterministic. Settlement can happen asynchronously after that, but settlement gates, semaphores, and per-user locks make sure the background workers do not corrupt the order that the engine already decided.





# Admission: 
## Validate that users have sufficient fresh on-chain balance, net of hard locks, to cover each new order. A certain percentage of incoming orders are intentionally oversized and must be rejected.

#### Note: the current service/contract model has no explicit maker/taker fees

FLOW:
POST `/orders` -> admission ticket -> wait for admission turn
After the request reaches its turn, check the user's balance cache. If the cache is missing, dirty, or too old, refresh token and vault balances from chain.
Admission rejects zero size/price, reservation overflow, stale balance cache after attempted refresh, or insufficient hard-available balance.
Buy reserve is `ceil(price * size / WAD)`; sell reserve is `size`.
Market orders are treated as hard risk: a live market order hard-locks its full remaining required balance.
A new limit order still has to be individually affordable against the user's current real balance minus hard locks, so an obviously oversized limit order is rejected. But resting limit orders do not hard-lock the full remaining balance for future admission.

How does your balance function when you place an order? 
### MKT orders
Market orders are admitted only if the full requested market-order reservation fits against current real balance minus hard locks. Once accepted, the market order hard-locks its remaining required balance. Resting limit orders can be overbooked, but after a market order is accepted the engine prunes over-reserved sibling orders by marking eligible live orders stale.

Sell orders use `size` as the reservation amount; there is no separate close-position exception in the engine.
### LMT orders
Limit orders add their full notional/base requirement to `reserved` at placement, but resting limits are not hard-locked for later admission. That means a user with $100 can place multiple individually affordable limits, such as 10 x $90, and become over-reserved. Before settlement and after balance refreshes/fills, the service prunes or stales live orders when refreshed `reserved > real`; otherwise remaining orders that still fit can continue matching.


# Order book + matching

## Price-time priority. Limit orders rest and market orders cross immediately.

Most of this logic lives in `service/src/engine`, especially `orders.rs`, `matching.rs`, and `book.rs`.

The engine stores full order state in:

- `orders: HashMap<String, Order>`

Book indexes are limit-order queues keyed by price:

- `bids: BTreeMap<U256, VecDeque<String>>`
- `asks: BTreeMap<U256, VecDeque<String>>`

We use `BTreeMap` because it keeps prices sorted. For bids we can walk prices from highest to lowest, and for asks we can walk prices from lowest to highest. Each price level stores order ids in a `VecDeque`, which gives FIFO ordering within the same price.

Earlier, if we only had:

```rust
orders: HashMap<String, Order>
balances: HashMap<Address, BalanceState>
next_order_seq: u64
next_fill_seq: u64
stats: Stats
```

then matching required scanning all buys against all sells:

```rust
for buy in self.orders.values().filter(...) {
    for sell in self.orders.values().filter(...) {
        ...
    }
}
```

That is `O(B*S)`, worst case `O(N^2)`, and it does too much work under load.

With the book indexes, inserts are `O(log P)` for price level lookup, where `P` is the number of price levels. Matching an incoming buy walks the lowest asks; matching an incoming sell walks the highest bids. In the common case this is much better because matching starts at the best price and FIFO queue instead of comparing every buy to every sell. The indexes may contain stale or in-flight ids until lazy cleanup, but snapshots and matching only use live, available limit orders.

The current flow is now: match synchronously, settle asynchronously.

For market orders, `POST /orders` immediately walks the opposite book best-price-first, creates fill candidates, and cancels any unfilled remainder before returning. Market orders never rest in the book and are not returned from `GET /orders` while waiting for settlement. If a market order matched, the service may keep an internal in-flight order record so settlement success/revert/abort can safely update reservations and fill state.

For limit orders, `POST /orders` immediately crosses any marketable quantity against the opposite book. The order is indexed after matching, but only the remaining available quantity is visible/resting in book depth; in-flight matched quantity is hidden.

The tradeoff is that settlement can still fail or be aborted after matching, because balances can change before the on-chain `Vault.matchOrders(...)` transaction lands, or because tx send, receipt, revert, or unknown-outcome handling fails. So the service still needs pre-settlement refresh, dirty marking, stale orders, and revert handling. But market-order behavior is now cleaner: matching/canceling happens immediately, while chain settlement remains async.



### Settlement: Call Vault.matchOrders(buyer, seller, quoteAmount, baseAmount) using the operator key from config. Handle reverts and unknown receipts.

This is where we go from off-chain matching to on-chain settlement.

The matching engine creates a fill candidate, which later gets submitted as:

vault.matchOrders(buyer, seller, quote, base).send().await

Before sending this transaction, the service does a pre-settlement balance refresh. It re-checks both users' on-chain balances because balances can change between admission, matching, and settlement.

If either side is still underfunded after refresh/pruning, the service does not submit the transaction and does not actually settle the orders.

If both sides still have sufficient funds, the service sends the transaction and waits for the receipt. The receipt can resolve in three main ways:

1. Success: the transaction landed and the service applies the fill.
2. Confirmed revert: the transaction failed on-chain. The service does not rely on a specific revert reason.
3. Uncertain: the service has a transaction hash, but cannot yet prove whether it succeeded or reverted.

The uncertain case is important. The service keeps the fill pending and reservations locked while it rechecks the transaction receipt. If later checks prove success, it applies the fill. If they prove revert, it aborts/stales according to policy. If the receipt remains unresolved after the deferred timeout, it marks both users dirty, stales both orders, aborts the fill, and releases the reservations. If it unlocked the funds too early, another order could use the same balance while the original transaction later succeeds on-chain, which would break the service's accounting.


### Balance reconciliation: On-chain balances change constantly underneath you. Your service needs a strategy for keeping its view fresh enough to make good admission decisions without polling every user every tick.

You obviously cannot hit the chain for every user on every tick. But caching balances is dangerous because stale balance data can make you admit orders that are no longer fundable. So the important thing is not just “use a cache”; it is “use a cache that has strong stale/dirty flags and only trust it when those flags say it is safe.”

When a user submits an order, we check whether their cached balance is admission-fresh.

A cache entry is admission-fresh if it exists, is not dirty, and was refreshed recently enough. If the cache is admission-fresh, we use it for admission.

A cache entry needs an admission refresh if it does not exist, is marked dirty, or is too old. If it needs refresh, we re-query the chain for that user's ERC20 balance and Vault balance, update the cache, and then use the refreshed balance for admission.

Users with reserved balances are higher risk because they have open orders or in-flight fills. Because of that, the service also has an active background refresh loop. It runs on a short interval and only picks users whose cache is dirty or too old. Dirty users are prioritized first, then the oldest cache entries.

This helps optimistically avoid the case where a user places an order, we admit it based on a good cache, but then the cache goes stale before settlement and we have to abort the settlement. We still have to handle that case, but the background refresh reduces how often it happens and removes some delay when settlement gets there.

The service also does log-based dirty marking. It polls chain logs for token/vault activity, like transfers, matches, and withdrawals. When one of those events touches a known user, the service marks that user’s cached balance as dirty.

Dirty does not mean “refresh every user immediately.” It means “do not fully trust this cached balance anymore.” The next admission or settlement path refreshes before relying on it. The active refresh loop also refreshes dirty users, but only among users with reserved balances. The balance-view endpoint reads fresh chain values for its response, but it does not clear or update the cached entry.

Finally, before settlement submit, the service does a pre-settlement refresh for both the buyer and the seller. In concurrent settlement mode this happens during the pre-submit check before the ordered tx-submit gate, so it is a pre-submit safety check rather than always literally the final instruction before `matchOrders`. Even if admission used a good cache, balances can still change before the on-chain transaction lands, so settlement re-checks both users before sending the transaction.
