# GPT was used for formatting and light copy editing; the substance is otherwise written by me
- Small note as I am not very familiar with Rust: I made a point to keep non-test files under `service/src` below 500 lines.
The dedicated test files are larger, and production modules only include small `#[cfg(test)]` hooks to point at those test files. This helped me review the implementation line by line and understand the generated output.
Also, my eyesight is lowkey dying, so I tend to log some things with color and formatting because it makes runtime output easier for me to scan.

 ^ if you run the run.sh script you will see what I mean

Here are metrics for an 8 min run
## First Interval Run

### First Stats Snapshot

| Metric | Value |
|---|---:|
| Orders received | 597 |
| Orders accepted | 475 / 597 (79.6%) |
| Orders rejected | 122 / 597 (20.4%) |
| Admission failures | 122 |
| Bad request | 0 |
| Insufficient balance | 122 |
| Stale cache | 0 |
| Refresh failures | 0 |
| Orders matched | 230 / 475 (48.4%) |
| Fill-side events | 372 |
| Fill candidates | 199 |
| Settlements attempted | 199 / 199 (100.0%) |
| Precheck passed | 199 / 199 (100.0%) |
| Precheck failed | 0 / 199 (0.0%) |
| Tx attempts | 199 / 199 (100.0%) |
| Tx submitted | 199 / 199 (100.0%) |
| Settlements reverted | 0 |
| Success | 186 |
| Pending | 13 |
| Currently open orders | 242 |
| Open status | 214 |
| Partial status | 28 |
| Lifetime accepted pct | 50.9% |

### Final Stats Snapshot

| Metric | Value |
|---|---:|
| Orders received | 26,104 |
| Orders accepted | 22,423 / 26,104 (85.9%) |
| Orders rejected | 3,681 / 26,104 (14.1%) |
| Admission failures | 3,681 |
| Bad request | 0 |
| Insufficient balance | 3,677 |
| Stale cache | 4 |
| Refresh failures | 0 |
| Orders matched | 10,414 / 22,423 (46.4%) |
| Fill-side events | 19,234 |
| Fill candidates | 9,717 |
| Settlements attempted | 9,698 / 9,717 (99.8%) |
| Precheck passed | 9,695 / 9,698 (100.0%) |
| Precheck failed | 3 / 9,698 (0.0%) |
| Tx attempts | 9,695 / 9,695 (100.0%) |
| Tx submitted | 9,695 / 9,695 (100.0%) |
| Settlements reverted | 17 / 9,695 (0.2%) |
| Success | 9,617 |
| Pending | 61 |
| Unattempted | 19 |
| Currently open orders | 579 |
| Open status | 569 |
| Partial status | 10 |
| Lifetime accepted pct | 2.6% |

The harness intentionally makes 25% of submitted orders oversized at `1.5x-3x` the user’s EOA balance. But “oversized” is based on raw size, while buy admission is based on notional. So a buy with `2x` balance in size at a `0.40` price only requires `0.8x` balance and can validly pass.

The larger tradeoff is that resting limit orders are only required to be individually affordable against real balance minus hard locks. We do not hard-lock the full notional for every open limit order, so users can quote more liquidity than they could settle all at once. That makes the book deeper and more flexible, but it also means some resting orders may become stale after fills or balance changes.

Hard-locking every limit order would improve safety and book quality, but it would also reject more orders and make the market thinner. In a very liquid market, stricter hard locks may make sense. Here, we are deliberately trading some safety and occasional book quality for better available liquidity.





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

- I honestly think I could make a strong argument for both sides of a lot of these choices so I will do my best to explain why I chose the ones I did and why I think for this the tradeoffs are worth it.


# Harness edits

The harness changes are limited to connection pooling and runtime control; they do not change the service HTTP API. The upstream harness created HTTP and RPC connections too aggressively under concurrency, which could cause runner crashes or timeouts from too many individual connections. To fix that, the harness now uses a shared `HarnessClients` struct with reusable `service: reqwest::Client` and `rpc: alloy::transports::http::reqwest::Client` clients, both configured with a 5 second timeout and a larger idle connection pool so setup, provider/reader creation, order loops, and chain loops can run higher concurrency more reliably.




# General concurrency things
The main concurrency change is that slow blockchain settlement work was moved out of the POST /orders path. The harness now reuses pooled HTTP/RPC clients, so it can generate high-concurrency load without wasting time on connection setup. On the service side, order admission and matching are still sequenced through admission tickets and the engine mutex, which keeps order IDs, fill IDs, book mutation, and price-time priority deterministic even when requests arrive concurrently.

Market orders now cross immediately and cancel any leftover size, while marketable limit orders also match immediately. The HTTP path creates fill candidates and pushes them to async settlement workers. Those workers handle balance refreshes, Vault.matchOrders(...), and receipts in the background, using bounds like semaphores, per-user locks, and apply gates so settlement can run concurrently without corrupting the book state chosen by the matching engine.

Admission tickets are just a FIFO gate for POST /orders.





# Ghost Orders And Limitations


A ghost order is an order that matches off-chain but cannot settle on-chain. This happens because the service only has a snapshot of user balances. A user can have enough balance when an order is admitted, then transfer funds or withdraw from the vault before Vault.matchOrders(...) lands, causing the settlement transaction to revert.

The service reduces this risk in several ways: it refreshes stale balances before admission, sequences admission before checking freshness, refreshes users with reserved balances in the background, marks users dirty from chain logs, refreshes buyer and seller balances again before settlement, skips transaction submission when a fill is already underfunded, and handles reverts by refreshing, pruning, releasing, or staling affected orders. If a transaction hash exists but the receipt outcome is unknown, the fill stays locked while the service rechecks; after a bounded timeout, both orders are staled and reservations are released.

The main unsolved weakness is that the pre-settlement refresh is not atomic with the on-chain transaction. A user can still move funds after the refresh but before settlement. That cannot be fully solved off-chain. A production system would likely need escrow or on-chain reservation before matching.

Remaining tradeoffs: resting limit orders can overbook balance, which improves liquidity but can leave stale book liquidity after fills or balance changes. Balance freshness still depends on cache age, log polling, and dirty flags. Send failures without a transaction hash are handled conservatively but not durably tracked. The settlement queue and order state are in-memory, so process restarts lose queued and in-flight work. Receipt timeouts prevent funds from staying locked forever, but a late-successful transaction after timeout would require later reconciliation.

In production, I would persist orders, reservations, fills, submitted tx hashes, receipt outcomes, balance-read block numbers, and dirty-user block numbers in a database, then make settlement workers resume only from that durable state. Before settlement, the worker would refresh balances and record the block; after submitting, it would store the tx hash/nonce before waiting for a receipt. Chain log events would mark users dirty at specific blocks, and a balance refresh would only clear dirty state if it was read at or after that block. After a restart, the service could safely reconstruct live orders, locked funds, pending settlements, and users that need refresh instead of relying on in-memory state.

Other cool stuff I might try if I wanted to get another 0.1% and max this would be to add an explicit final `eth_call` simulation of the exact `Vault.matchOrders` transaction against the node’s "pending" state immediately before broadcast, after the service’s existing balance refresh and underfunded-fill pruning. That would catch many last-moment balance/allowance races locally, mark the affected users or fill dirty/stale, and skip sending a doomed transaction, turning some settlements_reverted or implicit send failures into measurable settlements_precheck_failed outcomes while keeping settlement_tx_attempts reserved for transactions the service actually broadcasts.



Essentially if this crashes right now you are fucked, if it crashes in prod env you need to be able to fully replay // restore the entire state




# MAIN SECTION: 
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
