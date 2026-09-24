import { useEffect, useMemo, useState } from 'react'
import Chart from './Chart'
import { Metric } from './Primitives'
import { apiJson } from '../lib/api'

function numberValue(value, fallback = 0) {
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : fallback
}

function formatMoney(value) {
  if (value == null || !Number.isFinite(Number(value))) return '--'
  return new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD', maximumFractionDigits: 2 }).format(Number(value))
}

function formatPct(value) {
  if (value == null || !Number.isFinite(Number(value))) return '--'
  return `${Number(value).toFixed(1)}%`
}

function clone(value) {
  return JSON.parse(JSON.stringify(value))
}

export default function ResearchLab({ catalog, defaultSymbol, pricingMode, dealerModel }) {
  const dates = catalog?.common_dates || []
  const [symbol, setSymbol] = useState(defaultSymbol || 'SPY')
  const [startDate, setStartDate] = useState('')
  const [endDate, setEndDate] = useState('')
  const [entryMinute, setEntryMinute] = useState('10:00')
  const [exitMinute, setExitMinute] = useState('15:45')
  const [targetDte, setTargetDte] = useState(14)
  const [holdDays, setHoldDays] = useState(5)
  const [quantity, setQuantity] = useState(1)
  const [commission, setCommission] = useState(0.65)
  const [slippage, setSlippage] = useState(0.05)
  const [takeProfit, setTakeProfit] = useState(0.5)
  const [stopLoss, setStopLoss] = useState(0.5)
  const [exitDte, setExitDte] = useState(3)
  const [legs, setLegs] = useState([
    { right: 'PUT', side: 'BUY', target_delta: 0.30, ratio: 1 },
    { right: 'PUT', side: 'SELL', target_delta: 0.15, ratio: 1 },
  ])
  const [candidateGrid, setCandidateGrid] = useState('0.30,0.15\n0.25,0.10\n0.35,0.20')
  const [trainSessions, setTrainSessions] = useState(60)
  const [testSessions, setTestSessions] = useState(20)
  const [anchored, setAnchored] = useState(true)
  const [bootstrapIterations, setBootstrapIterations] = useState(2000)
  const [inferenceAlpha, setInferenceAlpha] = useState(0.05)
  const [initialCapital, setInitialCapital] = useState(100000)
  const [maxRiskPerTrade, setMaxRiskPerTrade] = useState(5)
  const [maxTotalRisk, setMaxTotalRisk] = useState(25)
  const [maxOpenPositions, setMaxOpenPositions] = useState(10)
  const [holdoutSessions, setHoldoutSessions] = useState(20)
  const [rollDte, setRollDte] = useState(3)
  const [rollTargetDte, setRollTargetDte] = useState(30)
  const [rollingCampaignSessions, setRollingCampaignSessions] = useState(60)
  const [maxRolls, setMaxRolls] = useState(2)
  const [holdoutPlan, setHoldoutPlan] = useState(null)
  const [holdoutPlanInput, setHoldoutPlanInput] = useState(null)
  const [holdoutResult, setHoldoutResult] = useState(null)
  const [result, setResult] = useState(null)
  const [regime, setRegime] = useState(null)
  const [walkForward, setWalkForward] = useState(null)
  const [stability, setStability] = useState(null)
  const [inference, setInference] = useState(null)
  const [portfolio, setPortfolio] = useState(null)
  const [rolling, setRolling] = useState(null)
  const [attribution, setAttribution] = useState(null)
  const [manifest, setManifest] = useState(null)
  const [status, setStatus] = useState('')
  const [error, setError] = useState('')

  useEffect(() => {
    if (defaultSymbol) setSymbol(defaultSymbol)
  }, [defaultSymbol])

  useEffect(() => {
    if (!dates.length) return
    setStartDate((current) => current || dates[Math.max(0, dates.length - 120)] || dates[0])
    setEndDate((current) => current || dates.at(-1))
  }, [dates.length])

  const request = useMemo(() => ({
    symbol,
    start_date: startDate || null,
    end_date: endDate || null,
    entry_minute: entryMinute,
    exit_minute: exitMinute,
    hold_trading_days: numberValue(holdDays, 5),
    target_dte: numberValue(targetDte, 14),
    quantity: numberValue(quantity, 1),
    pricing_mode: pricingMode || 'micro',
    dealer_model: dealerModel || 'classic',
    costs: {
      commission_per_contract: numberValue(commission),
      slippage_per_contract: numberValue(slippage),
    },
    exits: {
      take_profit_pct_of_risk: takeProfit === '' ? null : numberValue(takeProfit),
      stop_loss_pct_of_risk: stopLoss === '' ? null : numberValue(stopLoss),
      exit_dte_lte: exitDte === '' ? null : numberValue(exitDte),
    },
    legs: legs.map((leg) => ({
      ...leg,
      target_delta: numberValue(leg.target_delta),
      ratio: numberValue(leg.ratio, 1),
    })),
  }), [symbol, startDate, endDate, entryMinute, exitMinute, holdDays, targetDte, quantity, pricingMode, dealerModel, commission, slippage, takeProfit, stopLoss, exitDte, legs])

  const candidates = useMemo(() => {
    const lines = candidateGrid.split(/\n+/).map((line) => line.trim()).filter(Boolean)
    const parsed = []
    for (const line of lines) {
      const deltas = line.split(',').map((value) => Number(value.trim()))
      if (deltas.length !== legs.length || deltas.some((value) => !Number.isFinite(value))) continue
      const candidate = clone(request)
      candidate.legs = candidate.legs.map((leg, index) => ({ ...leg, target_delta: Math.abs(deltas[index]) }))
      parsed.push(candidate)
    }
    return parsed
  }, [candidateGrid, legs.length, request])

  const run = async (label, action) => {
    setStatus(label)
    setError('')
    try {
      return await action()
    } catch (reason) {
      setError(reason.message)
      return null
    } finally {
      setStatus('')
    }
  }

  const runBacktest = async () => {
    const data = await run('Running backtest', () => apiJson('/api/research/backtest', 'POST', request))
    if (!data) return
    setResult(data)
    setManifest(data.manifest)
    setAttribution(null)
  }

  const runRegime = async () => {
    const data = await run('Scanning regimes', () => apiJson('/api/research/regime-scan', 'POST', { backtest: request }))
    if (data) setRegime(data)
  }

  const freezeManifest = async () => {
    const data = await run('Freezing manifest', () => apiJson('/api/research/manifest', 'POST', request))
    if (data) setManifest(data)
  }

  const runWalkForward = async () => {
    if (!candidates.length) {
      setError('Candidate grid does not match the current number of legs')
      return
    }
    const data = await run('Running walk forward', () => apiJson('/api/research/walk-forward', 'POST', {
      candidates,
      start_date: startDate || null,
      end_date: endDate || null,
      train_sessions: numberValue(trainSessions, 60),
      test_sessions: numberValue(testSessions, 20),
      step_sessions: numberValue(testSessions, 20),
      min_train_trades: 10,
      anchored,
      selection_metric: 'average_pnl',
    }))
    if (data) setWalkForward(data)
  }

  const runStability = async () => {
    if (!candidates.length) {
      setError('Candidate grid does not match the current number of legs')
      return
    }
    const data = await run('Testing parameter stability', () => apiJson('/api/research/stability', 'POST', {
      candidates,
      start_date: startDate || null,
      end_date: endDate || null,
      min_trades: 10,
    }))
    if (data) setStability(data)
  }

  const runInference = async () => {
    if (!candidates.length) {
      setError('Candidate grid does not match the current number of legs')
      return
    }
    const data = await run('Running bootstrap inference', () => apiJson('/api/research/inference', 'POST', {
      candidates,
      bootstrap_iterations: numberValue(bootstrapIterations, 2000),
      alpha: numberValue(inferenceAlpha, 0.05),
      min_trades: 10,
    }))
    if (data) setInference(data)
  }

  const sealHoldout = async () => {
    const payload = {
      strategy: clone(request),
      start_date: startDate || null,
      end_date: endDate || null,
      holdout_sessions: numberValue(holdoutSessions, 20),
    }
    const data = await run('Sealing untouched holdout', () => apiJson('/api/research/holdout/seal', 'POST', payload))
    if (!data) return
    setHoldoutPlan(data)
    setHoldoutPlanInput(payload)
    setHoldoutResult(null)
  }

  const openHoldout = async () => {
    if (!holdoutPlan || !holdoutPlanInput) return
    const data = await run('Opening final holdout once', () => apiJson('/api/research/holdout/open', 'POST', {
      plan: holdoutPlanInput,
      commitment: holdoutPlan.commitment,
    }))
    if (data) setHoldoutResult(data)
  }

  const runRolling = async () => {
    const data = await run('Running rolling backtest', () => apiJson('/api/research/rolling', 'POST', {
      base: {
        ...request,
        hold_trading_days: numberValue(rollingCampaignSessions, 60),
      },
      roll_dte_lte: numberValue(rollDte, 3),
      roll_target_dte: numberValue(rollTargetDte, 30),
      max_rolls: numberValue(maxRolls, 2),
    }))
    if (data) setRolling(data)
  }

  const exportResearch = () => {
    const payload = {
      schema_version: 'option-workstation-research-export-v1',
      generated_at: new Date().toISOString(),
      symbol,
      manifest,
      backtest: result,
      attribution,
      regime,
      walk_forward: walkForward,
      parameter_stability: stability,
      inference,
      portfolio,
      rolling,
      holdout_plan: holdoutPlan,
      holdout_result: holdoutResult,
    }
    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const link = document.createElement('a')
    link.href = url
    link.download = `option-workstation-research-${symbol}-${new Date().toISOString().slice(0, 10)}.json`
    link.click()
    URL.revokeObjectURL(url)
  }

  const runPortfolio = async () => {
    const strategies = candidates.length ? candidates : [request]
    const data = await run('Running portfolio', () => apiJson('/api/research/portfolio', 'POST', {
      strategies,
      initial_capital: numberValue(initialCapital, 100000),
      max_open_positions: numberValue(maxOpenPositions, 10),
      max_risk_pct_per_trade: numberValue(maxRiskPerTrade, 5) / 100,
      max_total_open_risk_pct: numberValue(maxTotalRisk, 25) / 100,
      require_bounded_risk: true,
    }))
    if (data) setPortfolio(data)
  }

  const explainTrade = async (trade) => {
    const data = await run('Attributing P/L', () => apiJson('/api/research/attribution', 'POST', {
      symbol,
      entry_date: trade.entry_date,
      entry_minute: trade.entry_minute,
      exit_date: trade.exit_date,
      exit_minute: trade.exit_minute,
      expiration: trade.expiration,
      quantity: request.quantity,
      pricing_mode: request.pricing_mode,
      dealer_model: request.dealer_model,
      legs: trade.legs,
    }))
    if (data) setAttribution(data)
  }

  const addLeg = () => {
    if (legs.length >= 8) return
    setLegs([...legs, { right: 'CALL', side: 'BUY', target_delta: 0.30, ratio: 1 }])
  }

  const updateLeg = (index, key, value) => {
    setLegs(legs.map((leg, legIndex) => legIndex === index ? { ...leg, [key]: value } : leg))
  }

  const removeLeg = (index) => {
    if (legs.length <= 1) return
    setLegs(legs.filter((_, legIndex) => legIndex !== index))
  }

  const equityOption = result?.equity_curve?.length ? {
    animation: false,
    tooltip: { trigger: 'axis' },
    grid: { left: 54, right: 20, top: 18, bottom: 38 },
    xAxis: { type: 'category', data: result.equity_curve.map((point) => point.date), axisLabel: { color: '#83909c' } },
    yAxis: { type: 'value', axisLabel: { color: '#83909c' }, splitLine: { lineStyle: { color: '#1d2730' } } },
    series: [{ type: 'line', showSymbol: false, data: result.equity_curve.map((point) => point.cumulative_pnl) }],
  } : null

  const portfolioOption = portfolio?.mtm_equity_curve?.length ? {
    animation: false,
    tooltip: { trigger: 'axis' },
    grid: { left: 64, right: 20, top: 18, bottom: 38 },
    xAxis: { type: 'category', data: portfolio.mtm_equity_curve.map((point) => point.date), axisLabel: { color: '#83909c' } },
    yAxis: { type: 'value', axisLabel: { color: '#83909c' }, splitLine: { lineStyle: { color: '#1d2730' } } },
    series: [{
      name: 'MTM equity',
      type: 'line',
      showSymbol: false,
      connectNulls: false,
      data: portfolio.mtm_equity_curve.map((point) => point.complete ? point.equity : null),
    }],
  } : null

  return <div className="research-lab">
    <div className="research-lab-grid">
      <section className="research-config">
        <div className="research-section-title"><strong>Strategy Manifest</strong><span>{manifest?.strategy_id || 'unfrozen'}</span></div>
        <div className="research-form-grid">
          <label>Symbol<select value={symbol} onChange={(event) => setSymbol(event.target.value)}>{(catalog?.symbols || [symbol]).map((item) => <option key={item}>{item}</option>)}</select></label>
          <label>Start<input type="date" value={startDate} onChange={(event) => setStartDate(event.target.value)} /></label>
          <label>End<input type="date" value={endDate} onChange={(event) => setEndDate(event.target.value)} /></label>
          <label>Entry<input type="time" value={entryMinute} onChange={(event) => setEntryMinute(event.target.value)} /></label>
          <label>Exit<input type="time" value={exitMinute} onChange={(event) => setExitMinute(event.target.value)} /></label>
          <label>Target DTE<input type="number" min="0" value={targetDte} onChange={(event) => setTargetDte(event.target.value)} /></label>
          <label>Max hold<input type="number" min="1" value={holdDays} onChange={(event) => setHoldDays(event.target.value)} /></label>
          <label>Quantity<input type="number" min="1" value={quantity} onChange={(event) => setQuantity(event.target.value)} /></label>
        </div>

        <div className="research-section-title"><strong>Legs</strong><button onClick={addLeg}>Add leg</button></div>
        <div className="research-legs">
          {legs.map((leg, index) => <div className="research-leg" key={index}>
            <select value={leg.side} onChange={(event) => updateLeg(index, 'side', event.target.value)}><option>BUY</option><option>SELL</option></select>
            <select value={leg.right} onChange={(event) => updateLeg(index, 'right', event.target.value)}><option>PUT</option><option>CALL</option></select>
            <label>Δ<input type="number" min="0.01" max="0.99" step="0.01" value={leg.target_delta} onChange={(event) => updateLeg(index, 'target_delta', event.target.value)} /></label>
            <label>Ratio<input type="number" min="1" max="20" value={leg.ratio} onChange={(event) => updateLeg(index, 'ratio', event.target.value)} /></label>
            <button className="danger-text" onClick={() => removeLeg(index)}>Remove</button>
          </div>)}
        </div>

        <div className="research-section-title"><strong>Execution and exits</strong><span>all values become part of strategy ID</span></div>
        <div className="research-form-grid">
          <label>Commission / contract<input type="number" min="0" step="0.01" value={commission} onChange={(event) => setCommission(event.target.value)} /></label>
          <label>Extra slippage / contract<input type="number" min="0" step="0.01" value={slippage} onChange={(event) => setSlippage(event.target.value)} /></label>
          <label>Take profit / risk<input type="number" min="0" step="0.05" value={takeProfit} onChange={(event) => setTakeProfit(event.target.value)} /></label>
          <label>Stop loss / risk<input type="number" min="0" step="0.05" value={stopLoss} onChange={(event) => setStopLoss(event.target.value)} /></label>
          <label>Exit when DTE ≤<input type="number" min="0" value={exitDte} onChange={(event) => setExitDte(event.target.value)} /></label>
        </div>
        <div className="research-actions">
          <button onClick={freezeManifest} disabled={Boolean(status)}>Freeze manifest</button>
          <button className="primary" onClick={runBacktest} disabled={Boolean(status)}>Run backtest</button>
          <button onClick={runRegime} disabled={Boolean(status)}>Scan regimes</button>
        </div>
        {status && <div className="research-status">{status}…</div>}
        {error && <div className="research-error">{error}</div>}
      </section>

      <section className="research-config">
        <div className="research-section-title"><strong>Walk Forward</strong><span>{candidates.length} candidates</span></div>
        <label className="research-wide-label">Candidate delta rows
          <textarea value={candidateGrid} onChange={(event) => setCandidateGrid(event.target.value)} rows="5" />
          <small>One row per candidate. Deltas must match the leg count. Example for a two-leg spread: 0.30,0.15</small>
        </label>
        <div className="research-form-grid">
          <label>Train sessions<input type="number" min="10" value={trainSessions} onChange={(event) => setTrainSessions(event.target.value)} /></label>
          <label>Test sessions<input type="number" min="1" value={testSessions} onChange={(event) => setTestSessions(event.target.value)} /></label>
          <label className="research-checkbox"><input type="checkbox" checked={anchored} onChange={(event) => setAnchored(event.target.checked)} />Anchored training</label>
          <label>Bootstrap iterations<input type="number" min="200" max="20000" step="100" value={bootstrapIterations} onChange={(event) => setBootstrapIterations(event.target.value)} /></label>
          <label>Inference α<input type="number" min="0.001" max="0.49" step="0.01" value={inferenceAlpha} onChange={(event) => setInferenceAlpha(event.target.value)} /></label>
        </div>
        <div className="research-actions">
          <button className="primary" onClick={runWalkForward} disabled={Boolean(status)}>Run walk forward</button>
          <button onClick={runStability} disabled={Boolean(status)}>Parameter stability</button>
          <button onClick={runInference} disabled={Boolean(status)}>Bootstrap inference</button>
        </div>

        <div className="research-section-title portfolio-title"><strong>Final untouched holdout</strong><span>seal first, reveal once</span></div>
        <div className="research-form-grid">
          <label>Holdout sessions<input type="number" min="5" value={holdoutSessions} onChange={(event) => setHoldoutSessions(event.target.value)} /></label>
        </div>
        <div className="research-actions">
          <button onClick={sealHoldout} disabled={Boolean(status)}>Seal holdout</button>
          <button className="primary" onClick={openHoldout} disabled={Boolean(status) || !holdoutPlan || Boolean(holdoutResult)}>Open once</button>
          <button onClick={exportResearch}>Export research JSON</button>
        </div>
        {holdoutPlan && <div className="holdout-seal">
          <span>Commitment {holdoutPlan.commitment.slice(0, 20)}…</span>
          <span>Development through {holdoutPlan.development_end}</span>
          <span>Holdout {holdoutPlan.holdout_start} → {holdoutPlan.holdout_end}</span>
        </div>}

        <div className="research-section-title portfolio-title"><strong>Rolling engine</strong><span>close old contracts, reopen by target delta</span></div>
        <div className="research-form-grid">
          <label>Roll when DTE ≤<input type="number" min="0" value={rollDte} onChange={(event) => setRollDte(event.target.value)} /></label>
          <label>New target DTE<input type="number" min="1" value={rollTargetDte} onChange={(event) => setRollTargetDte(event.target.value)} /></label>
          <label>Campaign sessions<input type="number" min="1" max="120" value={rollingCampaignSessions} onChange={(event) => setRollingCampaignSessions(event.target.value)} /></label>
          <label>Max rolls<input type="number" min="1" max="12" value={maxRolls} onChange={(event) => setMaxRolls(event.target.value)} /></label>
        </div>
        <div className="research-actions"><button onClick={runRolling} disabled={Boolean(status)}>Run rolling backtest</button></div>

        <div className="research-section-title portfolio-title"><strong>Portfolio constraints</strong><span>uses the candidate strategies above</span></div>
        <div className="research-form-grid">
          <label>Initial capital<input type="number" min="1" step="1000" value={initialCapital} onChange={(event) => setInitialCapital(event.target.value)} /></label>
          <label>Risk / trade %<input type="number" min="0.1" max="100" step="0.5" value={maxRiskPerTrade} onChange={(event) => setMaxRiskPerTrade(event.target.value)} /></label>
          <label>Total open risk %<input type="number" min="0.1" max="100" step="1" value={maxTotalRisk} onChange={(event) => setMaxTotalRisk(event.target.value)} /></label>
          <label>Max open positions<input type="number" min="1" max="100" value={maxOpenPositions} onChange={(event) => setMaxOpenPositions(event.target.value)} /></label>
        </div>
        <div className="research-actions"><button className="primary" onClick={runPortfolio} disabled={Boolean(status)}>Run portfolio</button></div>
      </section>
    </div>

    {result && <section className="research-output">
      <div className="research-section-title"><strong>Backtest v2</strong><span>{result.manifest?.strategy_id}</span></div>
      <div className="compact-metrics">
        <Metric label="Trades" value={result.stats.trades} />
        <Metric label="Win rate" value={formatPct(result.stats.win_rate * 100)} />
        <Metric label="Net P/L" value={formatMoney(result.stats.total_pnl)} tone={result.stats.total_pnl >= 0 ? 'up' : 'down'} />
        <Metric label="Avg P/L" value={formatMoney(result.stats.average_pnl)} />
        <Metric label="Profit factor" value={result.stats.profit_factor?.toFixed(2)} />
        <Metric label="Max DD" value={formatMoney(result.stats.max_drawdown)} />
        <Metric label="Modeled costs" value={formatMoney(result.stats.total_costs)} />
      </div>
      {equityOption && <div className="research-chart"><Chart option={equityOption} viewKey={`research-backtest-${result.request_fingerprint}`} /></div>}
      <div className="research-table-wrap"><table className="research-table"><thead><tr><th>Entry</th><th>Exit</th><th>Reason</th><th>Gross</th><th>Costs</th><th>Net</th><th>Risk</th><th></th></tr></thead><tbody>
        {result.trades.slice(-30).reverse().map((trade, index) => <tr key={`${trade.entry_date}-${trade.exit_date}-${index}`}><td>{trade.entry_date}</td><td>{trade.exit_date}</td><td>{trade.exit_reason}</td><td>{formatMoney(trade.gross_pnl)}</td><td>{formatMoney(trade.total_costs)}</td><td className={trade.pnl >= 0 ? 'up' : 'down'}>{formatMoney(trade.pnl)}</td><td>{formatMoney(trade.risk_basis)}</td><td><button onClick={() => explainTrade(trade)}>Explain</button></td></tr>)}
      </tbody></table></div>
    </section>}

    {attribution && <section className="research-output">
      <div className="research-section-title"><strong>P/L Attribution</strong><span>Residual stays explicit</span></div>
      <div className="compact-metrics">
        <Metric label="Realized" value={formatMoney(attribution.realized_pnl)} />
        <Metric label="Delta" value={formatMoney(attribution.delta_effect)} />
        <Metric label="Gamma" value={formatMoney(attribution.gamma_effect)} />
        <Metric label="Theta" value={formatMoney(attribution.theta_effect)} />
        <Metric label="Vega" value={formatMoney(attribution.vega_effect)} />
        <Metric label="Residual" value={formatMoney(attribution.residual)} />
      </div>
    </section>}

    {regime && <section className="research-output">
      <div className="research-section-title"><strong>Regime scan</strong><span>{regime.total_trades} trades</span></div>
      <div className="research-table-wrap"><table className="research-table"><thead><tr><th>Dimension</th><th>Bucket</th><th>Trades</th><th>Win rate</th><th>Avg P/L</th><th>Total P/L</th></tr></thead><tbody>
        {regime.buckets.map((bucket) => <tr key={`${bucket.dimension}-${bucket.bucket}`}><td>{bucket.dimension}</td><td>{bucket.bucket}</td><td>{bucket.trades}</td><td>{formatPct(bucket.win_rate * 100)}</td><td>{formatMoney(bucket.average_pnl)}</td><td>{formatMoney(bucket.total_pnl)}</td></tr>)}
      </tbody></table></div>
    </section>}

    {walkForward && <section className="research-output">
      <div className="research-section-title"><strong>Walk Forward OOS</strong><span>{walkForward.folds.length} folds</span></div>
      <div className="compact-metrics">
        <Metric label="OOS trades" value={walkForward.out_of_sample_stats.trades} />
        <Metric label="OOS net P/L" value={formatMoney(walkForward.out_of_sample_stats.total_pnl)} tone={walkForward.out_of_sample_stats.total_pnl >= 0 ? 'up' : 'down'} />
        <Metric label="OOS avg P/L" value={formatMoney(walkForward.out_of_sample_stats.average_pnl)} />
        <Metric label="OOS PF" value={walkForward.out_of_sample_stats.profit_factor?.toFixed(2)} />
        <Metric label="Profitable folds" value={formatPct(walkForward.profitable_oos_folds_pct)} />
        <Metric label="OOS / train avg" value={walkForward.oos_to_selected_train_average_pnl_ratio?.toFixed(2)} />
      </div>
      <div className="research-table-wrap"><table className="research-table"><thead><tr><th>Fold</th><th>Train</th><th>Test</th><th>Strategy</th><th>Train avg</th><th>OOS avg</th><th>OOS P/L</th></tr></thead><tbody>
        {walkForward.folds.map((fold) => <tr key={fold.fold}><td>{fold.fold}</td><td>{fold.train_start} → {fold.train_end}</td><td>{fold.test_start} → {fold.test_end}</td><td className="mono">{fold.selected_strategy_id}</td><td>{formatMoney(fold.train_stats.average_pnl)}</td><td>{formatMoney(fold.test_stats.average_pnl)}</td><td>{formatMoney(fold.test_stats.total_pnl)}</td></tr>)}
      </tbody></table></div>
    </section>}

    {stability && <section className="research-output">
      <div className="research-section-title"><strong>Parameter Stability</strong><span>{stability.eligible_candidates} eligible neighbors</span></div>
      <div className="compact-metrics">
        <Metric label="Profitable neighbors" value={formatPct(stability.profitable_candidate_pct)} />
        <Metric label="Base-sign survival" value={formatPct(stability.base_sign_survival_pct)} />
        <Metric label="Median avg P/L" value={formatMoney(stability.median_average_pnl)} />
        <Metric label="Worst avg P/L" value={formatMoney(stability.worst_average_pnl)} />
        <Metric label="Best avg P/L" value={formatMoney(stability.best_average_pnl)} />
        <Metric label="Avg P/L σ" value={formatMoney(stability.average_pnl_stddev)} />
        <Metric label="Dispersion / mean" value={stability.dispersion_to_mean?.toFixed(2)} />
      </div>
      <div className="research-table-wrap"><table className="research-table"><thead><tr><th>Strategy</th><th>Trades</th><th>Avg P/L</th><th>vs Base</th><th>Win rate</th><th>PF</th><th>Max DD</th></tr></thead><tbody>
        {stability.candidates.map((candidate) => <tr key={candidate.strategy_id}><td className="mono">{candidate.strategy_id}</td><td>{candidate.trades}</td><td>{formatMoney(candidate.average_pnl)}</td><td>{candidate.average_pnl_vs_base?.toFixed(2) ?? '--'}</td><td>{formatPct(candidate.win_rate * 100)}</td><td>{candidate.profit_factor?.toFixed(2) ?? '--'}</td><td>{formatMoney(candidate.max_drawdown)}</td></tr>)}
      </tbody></table></div>
    </section>}

    {inference && <section className="research-output">
      <div className="research-section-title"><strong>Bootstrap Inference</strong><span>{inference.eligible_candidates}/{inference.tested_candidates} eligible</span></div>
      <div className="compact-metrics">
        <Metric label="α" value={inference.alpha?.toFixed(3)} />
        <Metric label="Iterations" value={inference.bootstrap_iterations} />
        <Metric label="Holm discoveries" value={inference.holm_discoveries} />
        <Metric label="BH FDR discoveries" value={inference.bh_fdr_discoveries} />
      </div>
      <div className="research-table-wrap"><table className="research-table"><thead><tr><th>Strategy</th><th>Trades</th><th>Avg P/L</th><th>Bootstrap CI</th><th>Raw p</th><th>Holm p</th><th>BH FDR p</th></tr></thead><tbody>
        {inference.candidates.map((candidate) => <tr key={candidate.strategy_id}><td className="mono">{candidate.strategy_id}</td><td>{candidate.trades}</td><td>{formatMoney(candidate.average_pnl)}</td><td>{candidate.bootstrap_ci_lower == null ? '--' : `${formatMoney(candidate.bootstrap_ci_lower)} → ${formatMoney(candidate.bootstrap_ci_upper)}`}</td><td>{candidate.raw_one_sided_p?.toFixed(4) ?? '--'}</td><td className={candidate.passes_holm ? 'up' : ''}>{candidate.holm_adjusted_p?.toFixed(4) ?? '--'}</td><td className={candidate.passes_bh_fdr ? 'up' : ''}>{candidate.bh_fdr_adjusted_p?.toFixed(4) ?? '--'}</td></tr>)}
      </tbody></table></div>
    </section>}

    {holdoutResult && <section className="research-output holdout-result">
      <div className="research-section-title"><strong>Final Holdout</strong><span>{holdoutResult.plan.commitment.slice(0, 20)}… opened</span></div>
      <div className="compact-metrics">
        <Metric label="Holdout trades" value={holdoutResult.holdout.stats.trades} />
        <Metric label="Net P/L" value={formatMoney(holdoutResult.holdout.stats.total_pnl)} tone={holdoutResult.holdout.stats.total_pnl >= 0 ? 'up' : 'down'} />
        <Metric label="Avg P/L" value={formatMoney(holdoutResult.holdout.stats.average_pnl)} />
        <Metric label="Win rate" value={formatPct(holdoutResult.holdout.stats.win_rate * 100)} />
        <Metric label="Profit factor" value={holdoutResult.holdout.stats.profit_factor?.toFixed(2)} />
        <Metric label="Max DD" value={formatMoney(holdoutResult.holdout.stats.max_drawdown)} />
        <Metric label="Costs" value={formatMoney(holdoutResult.holdout.stats.total_costs)} />
      </div>
    </section>}

    {rolling && <section className="research-output">
      <div className="research-section-title"><strong>Rolling Backtest</strong><span>{rolling.rolling_strategy_id}</span></div>
      <div className="compact-metrics">
        <Metric label="Campaigns" value={rolling.stats.campaigns} />
        <Metric label="Total rolls" value={rolling.stats.total_rolls} />
        <Metric label="Win rate" value={formatPct(rolling.stats.win_rate * 100)} />
        <Metric label="Net P/L" value={formatMoney(rolling.stats.total_pnl)} tone={rolling.stats.total_pnl >= 0 ? 'up' : 'down'} />
        <Metric label="Avg P/L" value={formatMoney(rolling.stats.average_pnl)} />
        <Metric label="Profit factor" value={rolling.stats.profit_factor?.toFixed(2)} />
        <Metric label="Max DD" value={formatMoney(rolling.stats.max_drawdown)} />
        <Metric label="Costs" value={formatMoney(rolling.stats.total_costs)} />
      </div>
      <div className="research-table-wrap"><table className="research-table"><thead><tr><th>Entry</th><th>Exit</th><th>Rolls</th><th>Initial expiry</th><th>Final expiry</th><th>Reason</th><th>Net P/L</th><th>Costs</th></tr></thead><tbody>
        {rolling.campaigns.slice(-30).reverse().map((campaign, index) => <tr key={`${campaign.entry_date}-${campaign.exit_date}-${index}`}><td>{campaign.entry_date}</td><td>{campaign.exit_date}</td><td>{campaign.roll_count}</td><td>{campaign.initial_expiration}</td><td>{campaign.final_expiration}</td><td>{campaign.exit_reason}</td><td className={campaign.pnl >= 0 ? 'up' : 'down'}>{formatMoney(campaign.pnl)}</td><td>{formatMoney(campaign.total_costs)}</td></tr>)}
      </tbody></table></div>
    </section>}

    {portfolio && <section className="research-output">
      <div className="research-section-title"><strong>Portfolio capital engine</strong><span>defined-risk admission</span></div>
      <div className="compact-metrics">
        <Metric label="Ending capital" value={formatMoney(portfolio.ending_capital)} />
        <Metric label="Net P/L" value={formatMoney(portfolio.net_pnl)} tone={portfolio.net_pnl >= 0 ? 'up' : 'down'} />
        <Metric label="Return" value={formatPct(portfolio.return_pct)} />
        <Metric label="Accepted" value={portfolio.accepted_trades} />
        <Metric label="Rejected" value={portfolio.rejected_trades} />
        <Metric label="Peak open risk" value={formatPct(portfolio.peak_open_risk_pct_of_equity)} />
        <Metric label="Realized max DD" value={formatPct(portfolio.max_realized_drawdown_pct)} />
        <Metric label="MTM max DD" value={formatPct(portfolio.max_mtm_drawdown_pct)} />
        <Metric label="MTM complete" value={portfolio.mtm_complete_points} detail={portfolio.mtm_missing_marks ? `${portfolio.mtm_missing_marks} missing marks` : 'all marks available'} />
        <Metric label="Modeled costs" value={formatMoney(portfolio.total_modeled_costs)} />
      </div>
      {portfolioOption && <div className="research-chart"><Chart option={portfolioOption} viewKey="research-portfolio" /></div>}
      <div className="research-reasons">{Object.entries(portfolio.rejection_reasons || {}).map(([reason, count]) => <span key={reason}>{reason}: {count}</span>)}</div>
    </section>}
  </div>
}
