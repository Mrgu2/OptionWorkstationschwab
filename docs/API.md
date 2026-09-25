# API Contract

Option Workstation exposes a local HTTP API on the configured loopback port.
The API is intended for the bundled frontend and local research automation; it
does not provide application-user authentication. Longbridge OAuth below is
provider authorization for the local live-data connection, not public user
login.

## Longbridge OAuth 2.0

`POST /api/oauth/start` starts a browser-based Longbridge authorization flow.
The request body is:

```json
{"client_id":"your-longbridge-oauth-client-id"}
```

The response contains a flow status and, once the local callback server is
ready, an `authorization_url`. It never contains an access token. The browser
should open that URL in a new tab and poll the status endpoint:

`GET /api/oauth/status`

The status is one of `idle`, `pending`, `connecting`, `connected`, or `error`.
The OAuth callback listens on `127.0.0.1:60355` and the token is held only by
the in-process SDK configuration. Tokens are not written to the OAuth crate's
default file storage. Starting a new flow or disconnecting cancels a pending
flow. This mechanism does not add authentication or authorization to the
HTTP API, so public or multi-user deployments remain unsupported.

## Point-in-time replay snapshot

`GET /api/v1/replay/snapshot` (the unversioned `/api/replay/snapshot` alias is
kept for the bundled frontend and pre-1.0 compatibility)

Query parameters:

| Parameter | Required | Description |
| --- | --- | --- |
| `symbol` | yes | Underlying symbol, for example `SPY` |
| `date` | yes | Trading date in `YYYY-MM-DD` |
| `minute` | yes | ET replay time in `HH:MM` |
| `expiration` | yes | Option expiration in `YYYY-MM-DD` |
| `pricing_mode` | no | `micro`, `mid`, or `ask`; defaults to `micro` |
| `dealer_model` | no | `classic`, `short_all`, or `long_all` |
| `max_dte` | no | Surface horizon from 1 to 1000 days; defaults to 180 |

The response is the authoritative replay unit:

```json
{
  "kind": "replay_snapshot",
  "snapshot_id": "replay:<chain-snapshot-id>",
  "symbol": "SPY",
  "date": "2026-07-10",
  "minute": "10:30",
  "expiration": "2026-07-17",
  "as_of": "2026-07-10T14:30:00Z",
  "model_version": "BSM+SVI-v1",
  "chain": {},
  "surface": {},
  "volatility": {}
}
```

The `chain`, `surface`, and `volatility` objects are computed from the same
symbol, date, minute and expiration request. Clients should display the
top-level `snapshot_id`, `as_of`, and `model_version` alongside derived panels.
The older `/api/chain`, `/api/surface`, and `/api/volatility-context` routes
remain available for compatibility.

## Error contract

Non-2xx responses use:

```json
{
  "detail": "human-readable explanation",
  "retry_after_ms": null
}
```

`409` means the requested state is not currently available, `429` means a
provider rate-limit window is active, and `502` means the upstream broker
operation failed. The frontend must preserve the last valid live snapshot for
these transient states and show the current connection state.

## Provenance expectations

Every derived response should expose, directly or through its parent snapshot:

- provider and quote interval;
- observation timestamp and timezone conversion;
- freshness and coverage;
- model and risk-free-rate assumptions;
- explicit unavailable, partial, or research-only reasons.

This contract intentionally does not expose licensed raw market data through a
public endpoint.


## Research lab APIs

The research lab endpoints operate only on locally available replay data and do
not submit orders.

### Backtest

`POST /api/research/backtest`

Runs a point-in-time options strategy backtest. Contracts are selected from the
entry snapshot by target delta and are then held as the same contracts until the
configured exit session. Buys use ask prices, sells use bid prices, and exit
liquidation uses the opposite executable side. Expiration selection is bounded:
0 DTE requires an exact same-day expiry, short-dated targets use tight calendar
tolerances, and longer-dated targets never substitute an expiry more than ten
calendar days away. Sessions without a sufficiently close expiry are skipped
instead of silently changing the strategy's maturity exposure.

The request supports:

- symbol and optional start/end dates;
- entry and exit minute;
- target DTE;
- maximum hold period in available trading sessions;
- quantity;
- one to eight call/put legs with side, target delta, and ratio;
- explicit commission and additional slippage per contract per execution;
- optional take-profit, stop-loss, and DTE exit rules.

The response includes every trade, skipped sessions, cumulative P/L, win rate,
profit factor, median P/L, maximum drawdown, total modeled execution costs, and
entry-session completion coverage. Attempted, completed, and skipped entry
counts are reported explicitly so missing historical quotes cannot disappear
from the performance summary.
Each trade exposes gross P/L, net P/L, entry/exit costs, holding sessions, risk
basis, and exit reason. The response also records the entry ATM IV, net GEX,
gamma-flip relationship, RR25, BF25, and quote quality so later regime analysis
can be reproduced.

### Strategy manifest

`POST /api/research/manifest`

Freezes a backtest strategy definition into a deterministic manifest and
`strategy_id`. Evaluation start/end dates are deliberately excluded from the
strategy identity. Changing a strategy parameter such as target delta, DTE,
quantity, exit rules, or cost assumptions changes the ID.

Backtest responses embed the same manifest so train and test runs can prove
which strategy definition was evaluated.

### P/L attribution

`POST /api/research/attribution`

Explains realized option P/L between two replay snapshots using entry delta,
gamma, theta, and vega. Vanna and charm are returned as diagnostics. They are not
added to explained P/L in v1 because doing so naively can double-count the same
spot/volatility/time interaction. Any unexplained amount remains visible as a
residual.

### Walk-forward validation

`POST /api/research/walk-forward`

Evaluates one to fifty candidate strategy definitions through sequential
training and out-of-sample test windows. Candidate ranking happens only inside
the training window. The selected strategy manifest is frozen before the test
window is evaluated.

The request supports anchored or rolling training windows, configurable train
and test session counts, non-overlapping step sizes, a minimum training-trade
requirement, and selection by average P/L, profit factor, or total P/L.

The response returns every fold, all candidate training scores, the selected
strategy ID, train and test statistics, aggregate out-of-sample statistics,
selection frequency, profitable OOS fold percentage, and the ratio between OOS
average P/L and the selected strategies' training average P/L.

Test windows are required to be non-overlapping. Trades are constrained to the
active fold window so an exit cannot consume prices from a later fold.

### Final untouched holdout

`POST /api/research/holdout/seal`

Reserves the final N trading sessions for one frozen strategy definition. New
seals use protocol `untouched-holdout-v2`. The SHA-256 commitment binds the
strategy ID, resolved development and holdout boundaries, a SHA-256 content
fingerprint of the replay Parquet files inside the holdout window, the configured
risk-free rate, and the declared research-engine contract. The response exposes
these identities and the frozen strategy definition, but no holdout performance.

The seal is also written to the append-only audit ledger. Only one active final
holdout may exist per symbol. While the seal remains unopened, research
endpoints for the same symbol are blocked from overlapping the reserved holdout
window. This includes backtests, P/L attribution, regime scans, walk-forward
validation, stability analysis, bootstrap inference, portfolio simulation, and
rolling backtests. The lock is calendar based so changing one strategy
parameter cannot silently expose the reserved sample.

`POST /api/research/holdout/open`

Loads the resolved holdout boundary from the sealed audit record, verifies the
strategy ID and commitment, clears replay caches, verifies the current replay
file bytes and engine configuration against the seal, runs the holdout, and
checks the replay fingerprint again before returning the result. The second
fingerprint check detects data mutation during evaluation. A commitment can be
opened only once.

Because the ledger stores the resolved boundary, adding newer replay dates after
a seal does not move the reserved sample. Legacy v1 seals remain readable and
openable once, but they cannot prove replay-file or engine identity because
those fields were not part of the v1 commitment.

The outer holdout sample dates and any dates already present on the strategy
request must agree. If only one location supplies a boundary, that boundary is
used. This prevents the API from silently sealing a different sample from the
one shown in the strategy configuration.

This protocol controls the workstation's research APIs for the sealed symbol.
It cannot erase knowledge already obtained outside the workstation or through
previous exposure to the same calendar period. It makes the declared final test
auditable and substantially reduces accidental holdout reuse inside the normal
research workflow.

### Parameter stability

`POST /api/research/stability`

Runs one to one hundred nearby strategy definitions over the same evaluation
window. The first candidate is the declared base strategy. The response reports
neighbor profitability, base-sign survival, median and worst average P/L,
cross-candidate dispersion, drawdown, and per-candidate statistics.

The candidate set should represent small parameter perturbations. Feeding
unrelated strategies into this endpoint weakens the interpretation of the
stability statistics.

### Statistical inference and multiple-testing controls

`POST /api/research/inference`

Runs deterministic trade-level bootstrap inference for one to one hundred
candidate strategy definitions. Each eligible candidate receives a percentile
bootstrap confidence interval for average P/L and a one-sided mean-centered
bootstrap p-value for the hypothesis that average P/L is greater than zero.

Because parameter searches create a family of simultaneous hypotheses, the
response also reports Holm-adjusted p-values for family-wise error control and
Benjamini-Hochberg adjusted p-values for false-discovery-rate control.

The request accepts the candidate strategies, bootstrap iteration count, alpha,
and minimum trade count. The default is 2,000 iterations, alpha 0.05, and ten
trades.

The bootstrap resamples completed trades as independent units. Serial
dependence, clustered volatility regimes, and overlapping exposures can make
trade-level intervals too narrow. These diagnostics quantify sampling
uncertainty and data-mining risk and should be interpreted alongside walk
forward validation, parameter stability, and the sealed final holdout.

### Rolling backtest

`POST /api/research/rolling`

Runs a separate rolling-strategy engine around a base backtest definition. The
request specifies a DTE trigger, a new target DTE, and a maximum number of
rolls. The base `hold_trading_days` field is the total rolling-campaign
horizon, so it should be long enough for the position to reach the roll trigger. When the trigger is reached, the engine liquidates the existing
contracts and opens newly delta-selected contracts at the same configured
minute.

Every roll records the old and new expiration, both leg sets, segment P/L, and
the close-plus-reopen execution costs. The report also exposes attempted,
completed, and skipped campaign counts plus a completion rate so missing marks
remain visible when evaluating rolling results. Take-profit and stop-loss checks occur
before the roll decision. The rolling strategy has its own deterministic ID so
its results cannot be confused with the non-rolling base strategy.

### Portfolio capital engine

`POST /api/research/portfolio`

Runs multiple backtest strategy definitions through one chronological capital
allocator. Each candidate trade is admitted only when it satisfies the
configured maximum open positions, per-trade risk cap, and aggregate open-risk
cap at that moment.

Defined-risk positions use the strategy max-loss estimate plus modeled execution
costs as capital at risk. By default, positions without a finite max-loss
estimate are rejected.

The response includes accepted and rejected trades, rejection reasons, ending
capital, realized return, peak open risk, realized drawdown, modeled costs,
per-strategy P/L contribution, and both realized and daily mark-to-market equity
curves.

For open positions the MTM engine replays the same selected option contracts at
the configured exit minute, values liquidation on the executable side, and
deducts hypothetical exit costs. Missing historical marks are never
forward-filled. Incomplete points are flagged and excluded from MTM drawdown
statistics.

### Strategy regime scan

`POST /api/research/regime-scan`

Runs the same point-in-time backtest and slices results by ATM IV regime, net GEX
sign, and whether spot was above or below gamma flip at entry. Regime buckets
are descriptive subsets of the same sample and are not independent validation.

### Trade journal replay

`GET /api/research/journal?symbol=SPY&limit=200`

Returns verified audit-ledger events in original ledger order. Optional symbol
filtering preserves snapshot IDs and the original recorded payload. Missing
decisions are never reconstructed as though they had been recorded.

## Candidate-family integrity

Walk-forward validation, parameter stability, bootstrap inference, and portfolio
simulation reject duplicate strategy definitions at the backend. Repeating the
same strategy does not create an additional hypothesis or an implicit second
copy of the position. Increase `quantity` explicitly when larger exposure is
intended.

Research requests also reject malformed or reversed date ranges, malformed
HH:MM times, non-finite numeric parameters, and unsupported pricing/dealer
modes. Unknown modes no longer fall back silently to another model.

## Research limitations

Backtest v2 models user-specified per-contract commissions and additional
slippage while executable option prices still use the adverse NBBO side. It
continues to omit taxes, early assignment, dividends, partial fills, broker
margin rules, and market impact beyond the configured slippage assumption. The
hold period is session-based and the contract selector targets delta.

Walk-forward validation reduces in-sample selection bias but does not eliminate
researcher degrees of freedom. Candidate grids, thresholds, universes, and
selection metrics should be declared before inspecting OOS results. The sealed
holdout protocol supplies a final auditable test after those decisions are
frozen.

Parameter-stability analysis measures whether nearby choices behave similarly;
it does not convert an in-sample pattern into independent evidence. Rolling
backtests also increase the number of modeled decisions and therefore require
the same walk-forward and holdout discipline.


## Research Lab UI

The workstation layout switch includes a Research view. It exposes the strategy
manifest, contract-selection rules, execution costs, deterministic exits,
backtest results, regime slices, walk-forward candidate grids, parameter
stability, sealed final holdouts, rolling-strategy tests, portfolio constraints,
daily MTM risk, and one-click P/L attribution for historical trades.

The UI is a client of the same local research APIs. Results retain the strategy
ID and engine assumptions so a visual experiment can be reproduced through the
API.


### Research result export

The Research UI can export a versioned JSON research bundle containing strategy
manifests and derived research reports currently visible in the UI. Raw option
chains and provider-owned replay files are not included in the export.
