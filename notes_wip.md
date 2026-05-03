# AJ - Vault Exercise submission

# 1. Overview
I used codex for formatting // spell check for this writeup but all initial versions of this were written by me before being cleaned up

Small note as I am not very familiar with Rust: I made a point to keep non-test files under `service/src` below 500 lines. This added some complexity in a few places where I think longer files (Particularly in the engine part) would have been easier, if I had to do it over I would take this into consideration and be a little more lenient with the file size.

Also, my eyesight is lowkey dying, so I tend to log some things with color and formatting because it makes runtime output easier for me to scan.
# 2. Repo Run Stats Comparison (50k order run)

| Metric | First run | Last run |
|---|---:|---:|
| Orders received | 844 | 50,556 |
| Orders accepted | 659 / 844 (78.1%) | 38,372 / 50,556 (75.9%) |
| Orders rejected | 185 / 844 (21.9%) | 12,184 / 50,556 (24.1%) |
| Admission failures | 185 | 12,184 |
| Insufficient balance rejects | 185 | 12,181 |
| Stale cache rejects | 0 | 3 |
| Orders matched | 336 / 659 (51.0%) | 18,150 / 38,372 (47.3%) |
| Fill side events | 590 | 35,142 |
| Settlements attempted | 303 / 303 (100.0%) | 17,674 / 17,681 (100.0%) |
| Precheck passed | 303 / 303 (100.0%) | 17,674 / 17,674 (100.0%) |
| Settlements reverted | 0 (0.0%) | 18 (0.1%) |
| Settlement success | 295 | 17,571 |
| Settlement pending | 8 | 85 |
| Settlement unattempted | 0 | 7 |
| Currently open orders | 306 | 10,492 |
| Open status | 281 | 10,212 |
| Partial status | 25 | 280 |
| Lifetime accepted pct open | 46.4% | 27.3% |
| Stored orders | 659 | 38,372 |
| Indexed book IDs | 325 | 10,945 |
| Pending engine fills | 0 | 0 |



## Notes about the adversarial harness
When placing orders, the harness builds a normal order payload by reading the user’s EOA balance, converting it into whole tokens, randomly choosing side/type/price, and then choosing a size.

In 25% of cases, it sets the raw size to 1.5x-3x the user’s token balance. But it does not mark that order as special or “oversized”; it just submits the normal payload with side, order_type, price, and WAD size.

The important mismatch is that buy admission does not check balance >= raw size. It checks:

required_balance = price * size
So a buy with 1.5x raw size can still pass if the price is low enough. For example, 1.5x size at 0.40 price only requires 0.60x balance.

Meaning we should not expect exactly 25% of all orders to reject. Some of the 25% “oversized” bucket can still be accepted, especially buys at lower prices. So 





# 3. Architecture

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

```mermaid
flowchart LR
    Client["HTTP client / harness"] --> Routes["routes.rs<br/>HTTP handlers"]
    Routes --> Sequencer["sequencing.rs<br/>AdmissionSequencer"]
    Sequencer --> Engine["engine/<br/>In-memory matching engine"]

    Engine --> Orders["engine/orders.rs<br/>Admission, reservation, cancel"]
    Engine --> Book["engine/book.rs<br/>Book indexes and snapshots"]
    Engine --> Matching["engine/matching.rs<br/>Crossing, fill candidates"]
    Engine --> Balances["engine/balances.rs<br/>Cached balances, dirty flags"]
    Engine --> Exposure["engine/exposure.rs<br/>Hard-locked funds"]

    Matching --> Settlement["tasks/settlement/<br/>Settlement workers"]
    Settlement --> Chain["chain.rs<br/>RPC, balances, matchOrders"]
    Chain --> Blockchain["Token + Vault<br/>contracts"]

    Chain --> BalanceRefresh["tasks.rs<br/>Balance refresh / log polling"]
    BalanceRefresh --> Balances
    Routes --> Stats["stats.rs + engine/snapshot.rs<br/>/stats snapshots"]
```

### Fill lifecycle
```mermaid
sequenceDiagram
    participant H as Harness / Client
    participant R as routes.rs
    participant S as AdmissionSequencer
    participant E as Engine
    participant Q as settlement_queue
    participant T as Settlement loop
    participant C as ChainClient
    participant B as Blockchain

    H->>R: POST /orders
    R->>E: balance_needs_admission_refresh(user)
    R->>C: read_user_balances(user), if needed
    C-->>R: token + vault balances at block
    R->>E: apply_balance_refresh_at_block()

    R->>S: issue_ticket()
    R->>S: ticket.wait_for_turn()

    R->>E: balance_needs_admission_refresh(user)
    R->>C: read_user_balances(user), if needed
    C-->>R: refreshed balances
    R->>E: apply_balance_refresh_at_block()

    R->>E: submit_order_and_claim_fills(request)
    E->>E: validate size, price, balance freshness
    E->>E: reserve funds by side
    E->>E: match_new_order()
    E-->>R: OrderAdmission { response, fills }

    loop each fill from admission
        R->>Q: settlement_queue.send(fill)
    end
    R-->>H: OrderResponse

    T->>Q: fill_rx.recv()
    Q-->>T: FillCandidate

    T->>E: fill_still_pending()
    T->>C: read_user_balances(buyer/seller)
    C-->>T: fresh balances
    T->>E: apply_balance_refresh_at_block()
    T->>E: prune_underfunded_fill_users()
    T->>E: record_settlement_tx_attempt()

    T->>C: submit_settlement_once()
    C->>B: Vault.matchOrders(buyer, seller, quote, base)
    B-->>C: receipt / revert / unknown
    C-->>T: confirmation result

    alt success
        T->>C: refresh_after_success()
        T->>E: apply_settlement_success()
        T->>E: claim_fill_batch()
        E-->>T: newly available fills
        T->>Q: settlement_queue.send(requeued fills)
    else revert or failed send
        T->>E: abort_fill()
        opt release/prune path can still match
            T->>E: claim_fill_batch()
            E-->>T: newly available fills
            T->>Q: settlement_queue.send(requeued fills)
        end
    else uncertain receipt
        T->>E: hold_unresolved_settlement()
        T->>C: settlement_receipt_status(tx_hash)
        alt later success
            T->>E: apply_settlement_success()
        else later revert
            T->>E: abort_fill()
        else timeout unresolved
            T->>E: time_out_unresolved_settlement()
        end
    end
```
# 4. Design choices 

Many of these design choices I believe have good arguments on both sides. I'll explain the reasoning behind my decisions and why, in this context, I believe the tradeoffs are worthwhile.


## Harness edits

The harness changes are limited to connection pooling and runtime control; they do not change the service HTTP API. The upstream harness created HTTP and RPC connections too aggressively under concurrency, which could cause runner crashes or timeouts from too many individual connections. To fix that, the harness now uses a shared `HarnessClients` struct with reusable `service: reqwest::Client` and `rpc: alloy::transports::http::reqwest::Client` clients, both configured with a 5 second timeout and a larger idle connection pool so setup, provider/reader creation, order loops, and chain loops can run higher concurrency more reliably.




## General concurrency things
The main concurrency change is that slow blockchain settlement work was moved out of the POST /orders path. The harness now reuses pooled HTTP/RPC clients, so it can generate high-concurrency load without wasting time on connection setup. On the service side, order admission and matching are still sequenced through admission tickets (Admission tickets are just a FIFO gate for POST /orders) and the engine mutex, which keeps order IDs, fill IDs, book mutation, and price-time priority deterministic even when requests arrive concurrently.

Market orders now cross immediately and cancel any leftover size, while limit orders also match immediately. The HTTP path creates fill candidates and pushes them to async settlement workers. Those workers handle balance refreshes, Vault.matchOrders(...), and receipts in the background, using bounds like semaphores, per-user locks, and apply gates so settlement can run concurrently without corrupting the book state chosen by the matching engine.



## Balance Accounting at Order Admission

This section describes how the engine decides whether a new order can enter the book. It is separate from settlement durability: these checks protect the live in-memory engine while the process is running.

For every order, the service uses a fresh-enough on-chain token balance, then checks the new order against the user's hard-available balance. Buy orders reserve `ceil(price * size / WAD)`, while sell orders reserve `size`. There is no separate close-position exception in the engine.

Hard locks are intentionally narrower than total reservations. Market orders and in-flight fills count as hard locks. Resting limit orders increase `reserved`, but they are not fully hard-locked against future limit-order admission.

### Market Orders

Market orders are admitted only if the full requested market-order reservation fits against current real balance minus hard locks. Once accepted, they cross immediately against available older resting limits. Any unmatched remainder is cancelled, while any matched in-flight amount stays hard-locked until settlement succeeds, fails, or is released.

Because market orders create immediate settlement risk, accepting one can make older resting limit orders no longer fit the user's real balance. When that happens, the engine prunes eligible over-reserved sibling orders by marking them stale.

### Limit Orders

Limit orders add their full notional or base requirement to `reserved` at placement, but resting limits are not hard-locked for later admission. This means a user with `$100` can place multiple individually affordable limits, such as ten `$90` orders, and become over-reserved.

That overbooking is deliberate because it lets users express multiple resting intents with the same balance, supports laddered quotes across price levels, and makes the book deeper for matching and price discovery. The tradeoff is that not every visible resting order is guaranteed to remain fundable after other fills or balance changes. The service handles this by treating market orders and in-flight fills as hard locks, then pruning or staling live orders after balance refreshes, settlement success, and failed settlement paths when refreshed `reserved > real`.



# 5. Ghost Orders and Limitations

A ghost order is an order that matches off-chain but cannot settle on-chain. In this service, that risk exists because matching is based on cached on-chain balances, while the actual settlement happens later through `Vault.matchOrders(...)`. A user may have enough token balance when an order is admitted or prechecked, then move funds or change allowance before the settlement transaction executes.

The service reduces this risk by refreshing stale balances before admission, sequencing admission through tickets, refreshing users with reserved balances in the background, marking users dirty from chain logs, and refreshing both buyer and seller again immediately before settlement. If the fill is already underfunded at that point, the service skips transaction submission. If settlement reverts or sending fails, it refreshes/marks dirty and either releases, prunes, or stales affected orders. If a transaction hash exists but the receipt is uncertain, the fill stays locked while the service rechecks; after a bounded timeout, both orders are staled and reservations are released.

### Remaining gap
Pre-settlement refresh is still separate from transaction execution. More frequent cache refreshes reduce stale admission and stale book liquidity, but they do not fully remove the final race between the last balance read and `Vault.matchOrders(...)` landing on-chain. Stronger production guarantees would likely require escrow, on-chain reservation, or another atomic commitment mechanism before matching.


### Order Design

Resting limit orders intentionally allow overbooking: each new limit only needs to be individually affordable against current real balance minus hard locks. This lets users place multiple resting intents or ladder quotes with the same balance, improving book depth and matching. The tradeoff is that some visible liquidity can become stale after fills or balance changes, so the service prunes or stales live orders after refreshes, successful fills, and failed settlement paths.

Another gap is durability. Orders, reservations, fill candidates, in-flight settlements, tx hashes, receipt outcomes, balance-read blocks, and dirty-user blocks are all in memory, so restart recovery is unsafe. In production, I would persist those records and resume settlement only from durable state.

A further improvement would be a final `eth_call` simulation of the exact `Vault.matchOrders(...)` call against pending state before broadcast. That would catch many last-moment balance or allowance failures and turn doomed transactions into precheck failures instead of settlement reverts.




# 6. Admission
## Validate that users have sufficient fresh on-chain balance, net of hard locks, to cover each new order. A certain percentage of incoming orders are intentionally oversized and must be rejected.



The service validates each new order against a fresh-enough on-chain token balance. The admission path is:

`POST /orders` -> issue admission ticket -> wait for turn -> refresh balance if missing, dirty, or too old -> validate and reserve.

Admission rejects zero size/price, reservation overflow, stale balance after attempted refresh, or insufficient hard-available balance.

In the current service/contract model there are no maker or taker fees, so reservation math is:

- Buy reserve: `ceil(price * size / WAD)`
- Sell reserve: `size`

Hard-available balance means real on-chain token balance minus hard locks. Market orders and in-flight fills are hard locks; resting limit orders are not fully hard-locked for future admission.

### Market Orders

Market orders must be fully affordable at admission. They cross immediately against available older resting limits. Any unmatched remainder is cancelled, while any matched in-flight amount stays hard-locked until settlement succeeds, fails, or is released.

Because market orders create immediate settlement risk, accepting one can make the user's older resting limits over-reserved. In that case, the engine prunes eligible sibling orders by marking them stale.

### Limit Orders

Limit orders add their full notional or base requirement to `reserved`, but resting limits are not hard locks for later limit admission. This means a user with `$100` can place multiple individually affordable resting limits, such as ten `$90` orders, and become over-reserved.

That overbooking is deliberate: it lets users ladder quotes and improves book depth. The tradeoff is that some visible liquidity can become stale after fills or balance changes, so the service prunes or stales live orders after refreshes, successful settlements, and failed settlement paths when refreshed `reserved > real`.

If fees were added later, they would need to be included in the reservation formula. For example, a buyer-side fee would make buy reserve roughly:

`ceil(price * size / WAD) + fee`

and a seller-side fee would require either reserving extra token balance or settling fees from proceeds, depending on the fee model.


# 7. Order Book + Matching

## Price-time priority. Limit orders rest and market orders cross immediately.

Most of this logic lives in `service/src/engine`, especially `orders.rs`, `matching.rs`, and `book.rs`.

The engine stores full order state in `orders: HashMap<String, Order>`. The public book is maintained through limit-order indexes keyed by price:

- `bids: BTreeMap<U256, VecDeque<String>>`
- `asks: BTreeMap<U256, VecDeque<String>>`

`BTreeMap` keeps prices sorted, so bids can be walked from highest to lowest and asks from lowest to highest. Each price level stores order ids in a `VecDeque`, which preserves FIFO ordering within the same price level. Matching and snapshots lazily clean stale index entries, so the indexes may temporarily contain filled, cancelled, stale, or in-flight order ids, but only live and available limit orders are used.

The current flow is: match synchronously, settle asynchronously. `POST /orders` refreshes admission balance if needed, waits for the admission ticket, submits the order through `submit_order_and_claim_fills(...)`, and immediately sends any generated `FillCandidate`s to the settlement queue. Settlement workers later refresh balances again, submit `Vault.matchOrders(...)`, confirm receipts, and apply success or abort/revert handling.

For market orders, `POST /orders` immediately walks the opposite book best-price-first and creates fill candidates against available older resting limits. Any unmatched remainder is cancelled before the response returns. Market orders never rest in the book and are hidden from `GET /orders` while their matched amount is waiting for settlement. If a market order matched, the engine keeps internal in-flight state so settlement success, revert, or abort can update reservations and fill state safely.

For limit orders, `POST /orders` immediately crosses any marketable quantity against the opposite book before indexing the new order. After that, the order is inserted into the limit-order index. Public book depth only counts live, available limit liquidity from users without an in-flight order, so in-flight matched quantity is not exposed as resting depth.

The tradeoff is that settlement can still fail after off-chain matching because balances or allowances can change before `Vault.matchOrders(...)` executes, or because transaction send, receipt, revert, or unknown-outcome handling fails. That is why the service still needs pre-settlement refresh, dirty marking, stale orders, requeue handling, and revert/abort paths. Market-order behavior is still clean from the client perspective: matching and remainder cancellation happen immediately, while chain settlement remains asynchronous.





# 9. Balance reconciliation: 
## On-chain balances change constantly underneath you. Your service needs a strategy for keeping its view fresh enough to make good admission decisions without polling every user every tick.

You obviously cannot hit the chain for every user on every tick. But caching balances is dangerous because stale balance data can make you admit orders that are no longer fundable. So the important thing is not just “use a cache”; it is “use a cache that has strong stale/dirty flags and only trust it when those flags say it is safe.”

When a user submits an order, we check whether their cached balance is admission-fresh.

A cache entry is admission-fresh if it exists, is not dirty, and was refreshed recently enough. If the cache is admission-fresh, we use it for admission.

A cache entry needs an admission refresh if it does not exist, is marked dirty, or is too old. If it needs refresh, we re-query the chain for that user's ERC20 balance and Vault balance, update the cache, and then use the refreshed balance for admission.

Users with reserved balances are higher risk because they have open orders or in-flight fills. Because of that, the service also has an active background refresh loop. It runs on a short interval and only picks users whose cache is dirty or too old. Dirty users are prioritized first, then the oldest cache entries.

This helps optimistically avoid the case where a user places an order, we admit it based on a good cache, but then the cache goes stale before settlement and we have to abort the settlement. We still have to handle that case, but the background refresh reduces how often it happens and removes some delay when settlement gets there.

The service also does log-based dirty marking. It polls chain logs for token/vault activity, like transfers, matches, and withdrawals. When one of those events touches a known user, the service marks that user’s cached balance as dirty.

Dirty does not mean “refresh every user immediately.” It means “do not fully trust this cached balance anymore.” The next admission or settlement path refreshes before relying on it. The active refresh loop also refreshes dirty users, but only among users with reserved balances. The balance-view endpoint reads fresh chain values for its response, but it does not clear or update the cached entry.

Finally, before settlement submit, the service does a pre-settlement refresh for both the buyer and the seller. In concurrent settlement mode this happens during the pre-submit check before the ordered tx-submit gate, so it is a pre-submit safety check rather than always literally the final instruction before `matchOrders`. Even if admission used a good cache, balances can still change before the on-chain transaction lands, so settlement re-checks both users before sending the transaction.
