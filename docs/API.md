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
liquidation uses the opposite executable side.

The request supports:

- symbol and optional start/end dates;
- entry and exit minute;
- target DTE;
- hold period in available trading sessions;
- quantity;
- one to eight call/put legs with side, target delta, and ratio.

The response includes every trade, skipped sessions, cumulative P/L, win rate,
profit factor, median P/L, and maximum drawdown. The response also records the
entry ATM IV, net GEX, gamma-flip relationship, RR25, BF25, and quote quality so
later regime analysis can be reproduced.

### P/L attribution

`POST /api/research/attribution`

Explains realized option P/L between two replay snapshots using entry delta,
gamma, theta, and vega. Vanna and charm are returned as diagnostics. They are not
added to explained P/L in v1 because doing so naively can double-count the same
spot/volatility/time interaction. Any unexplained amount remains visible as a
residual.

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

## Research limitations

Backtest v1 intentionally omits commissions, taxes, early assignment, dividends,
market impact, partial fills, and broker margin rules. The hold period is
session-based and the v1 contract selector targets delta. Any performance claim
must therefore state these assumptions and should use holdout or walk-forward
validation before treating a historical pattern as persistent edge.
