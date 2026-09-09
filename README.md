# Option Workstation · Schwab Market Data Fork

[![Rust](https://img.shields.io/badge/Rust-2024-000000?logo=rust&logoColor=white)](rust-backend/Cargo.toml)
[![React](https://img.shields.io/badge/React-19-61dafb?logo=react&logoColor=111111)](frontend/package.json)
[![Vite](https://img.shields.io/badge/Vite-7-646cff?logo=vite&logoColor=white)](frontend/package.json)
[![Schwab](https://img.shields.io/badge/Schwab-Market%20Data-00a0df)](https://developer.schwab.com/)
[![Docker](https://img.shields.io/badge/Docker-ready-2496ed?logo=docker&logoColor=white)](Dockerfile)
[![License](https://img.shields.io/badge/License-Apache--2.0-blue)](LICENSE)

这是 `Option Workstation` 的 Schwab Market Data 适配分支，目标很明确：

**只接 Schwab 行情数据，用现有 Rust 分析层做期权链、IV、Greeks、GEX、Vanna、Charm、SVI、曲面和策略风险研究。**

本 fork 不连接 Schwab Trader API，不读取真实账户、余额或持仓，也不提供下单、改单、撤单能力。

> [!WARNING]
> 本项目用于研究和学习，不构成投资建议。期权 Greeks、Dealer Exposure、SVI、Expected Move 等均包含模型假设。实时结果还取决于 Schwab 数据权限、行情延迟、网络状态和 API 限频。

## 当前数据边界

Schwab 适配器只调用 Market Data API：

- `GET /marketdata/v1/quotes`：验证 Market Data token；
- `GET /marketdata/v1/expirationchain`：取得可用到期日；
- `GET /marketdata/v1/chains`：取得期权链、OI、IV、Greeks 和 underlying quote；
- `GET /marketdata/v1/pricehistory`：取得分钟线和日线；
- OAuth token endpoint：交换和刷新 Market Data access token。

项目不会调用 `/trader/v1`，Rust Router 也不暴露 `/api/trade/*`。

`/api/health` 会明确返回 Schwab Market Data provider，并标记 `trading_enabled: false`。

## 核心能力

### 实时研究

- Schwab 期权链和 underlying quote；
- Bid / Ask、Volume、Open Interest；
- Schwab IV 与 Greeks；
- BSM fallback Greeks；
- GEX、Vanna、Charm；
- Call Wall、Put Wall、Gamma Flip；
- ATM IV、25Δ Risk Reversal、Butterfly；
- SVI smile 与约束波动率曲面；
- 多期限 term structure；
- Spot / IV / 时间情景下的多腿策略风险预览；
- 本地 WebSocket 推送标准化快照。

### 历史回放

原项目的本地 Parquet replay 能力继续保留。仓库不分发行情数据，历史数据需要由使用者自行合法取得并放到 `OPTION_WORKSTATION_DATA_ROOT`。

### 研究审计

关键研究快照可以写入本地 JSONL 审计账本。凭证不会写入审计记录。

## Schwab 数据质量保护

这个 fork 对 Schwab 返回值做了几层额外防护，避免看起来有数字但分析结果已经失真。

### 标准合约过滤

GEX 模型按标准美股期权 `100` 股 multiplier 计算，因此 Schwab chain 中以下合约会被排除：

- `mini = true`；
- `nonStandard = true`；
- 明确给出且不等于 `100` 的 multiplier。

这样可以避免调整合约、mini option 被套用标准合约 GEX multiplier。

### IV 单位

Schwab option chain 的 `volatility` 按百分比处理。例如：

```text
25.0  ->  0.25
3.5   ->  0.035
```

进入分析层后再统一转成内部小数口径。

### Greeks 异常值

Schwab / 兼容行情源在 Greeks 不可用时可能出现 provider sentinel。明显无效的 Greeks 会被丢弃，由现有 BSM 模型回退计算，避免异常 Gamma 直接污染 GEX。

### 延迟与截断

适配器读取：

- `isDelayed`；
- underlying quote timestamp；
- option quote timestamp；
- `isChainTruncated`。

质量状态会区分 ready、delayed、stale、truncated、quote coverage 不足和 metadata coverage 不足。

### 分析时间与行情时间分离

期权链的行情时间用于显示数据新鲜度；BSM / TTE / SVI 等分析使用当前计算时间。

这能避免休市、延迟行情或历史 quote timestamp 把剩余到期时间算错。

### 分钟图

实时分钟线优先显示 Schwab 返回的最新可用交易日，因此周末或休市时不会因为当前自然日没有 candle 而把图表清空。

图中的 VWAP 是根据分钟 OHLCV 计算的累计 typical-price VWAP proxy：

```text
Typical Price = (High + Low + Close) / 3
VWAP proxy = cumulative(Typical Price × Volume) / cumulative(Volume)
```

它用于工作台参考，不应理解为交易所官方逐笔 VWAP。

## OAuth 设置

### 1. 创建 Schwab Developer App

在 Schwab Developer Portal 创建应用，并启用你需要的 **Market Data** 产品。

本 fork 不需要 Trader API。

Callback URL 必须与本地配置逐字符一致。默认值：

```text
https://127.0.0.1:5556
```

### 2. 配置本地环境

```bash
cp .env.example .env
```

至少填写：

```dotenv
OPTION_WORKSTATION_SCHWAB_APP_SECRET=your_app_secret
OPTION_WORKSTATION_SCHWAB_REDIRECT_URI=https://127.0.0.1:5556
OPTION_WORKSTATION_SCHWAB_REFRESH_MS=3000
```

App Secret 只从 Rust 后端环境变量读取。Schwab UI 中的手动 App Secret / Access Token 表单在本 fork 中隐藏。

Access Token、Refresh Token 和 App Secret 只保存在当前 Rust 进程内存。服务重启后需要重新建立授权状态。

调用外部 `curl` 时，Bearer token、OAuth code、Refresh Token 和 Basic credentials 通过 stdin config 传入，不放到命令行参数中。

### 3. 完成 OAuth

进入实时模式后：

1. 打开连接面板；
2. 输入 Schwab App Key；
3. 点击开始 OAuth；
4. 在 Schwab 页面完成授权；
5. 浏览器跳转到 Callback URL；
6. 如果本地 Callback 页面无法打开，直接复制地址栏中的完整 URL；
7. 将完整 URL 粘贴进工作台的 `OAuth Callback URL` 输入框；
8. 点击 `完成 OAuth 连接`。

后端会从 callback URL 提取 authorization code，交换 token，并使用 Market Data `quotes` endpoint 验证连接。

OAuth 会话只存在于当前进程。如果服务在授权途中重启，工作台会要求重新开始 OAuth，避免拿丢失上下文的 callback code 继续交换。

## 本地运行

### 方式 A：开发模式

终端 1：

```bash
cp .env.example .env
# 编辑 .env

cargo run --manifest-path rust-backend/Cargo.toml
```

终端 2：

```bash
cd frontend
npm ci
npm run dev
```

浏览器打开：

```text
http://127.0.0.1:7310/?mode=live
```

Vite 会把 `/api` 代理到 Rust 服务的 `127.0.0.1:7311`。

### 方式 B：Rust 直接托管前端

```bash
cd frontend
npm ci
npm run build
cd ..

cargo run --manifest-path rust-backend/Cargo.toml
```

打开：

```text
http://127.0.0.1:7311/?mode=live
```

### 方式 C：Docker

```bash
docker build -t option-workstation-schwab .

docker run --rm \
  -p 127.0.0.1:7311:7311 \
  -e OPTION_WORKSTATION_SCHWAB_APP_SECRET='your_app_secret' \
  -e OPTION_WORKSTATION_SCHWAB_REDIRECT_URI='https://127.0.0.1:5556' \
  option-workstation-schwab
```

如果使用历史 replay 数据，再把数据目录挂载到 `/data`。

## 主要环境变量

| 变量 | 默认值 | 用途 |
| --- | --- | --- |
| `OPTION_WORKSTATION_HOST` | `127.0.0.1` | Rust 服务监听地址 |
| `OPTION_WORKSTATION_PORT` | `7311` | Rust 服务端口 |
| `OPTION_WORKSTATION_DATA_ROOT` | `./data` | 历史 replay 数据目录 |
| `OPTION_WORKSTATION_FRONTEND_DIST` | `./frontend/dist` | 前端构建目录 |
| `OPTION_WORKSTATION_RISK_FREE_RATE` | `0.043` | 分析层无风险利率 |
| `OPTION_WORKSTATION_SCHWAB_APP_SECRET` | 空 | Schwab App Secret |
| `OPTION_WORKSTATION_SCHWAB_REDIRECT_URI` | `https://127.0.0.1:5556` | Schwab OAuth Callback URL |
| `OPTION_WORKSTATION_SCHWAB_REFRESH_MS` | `3000` | Market Data REST 刷新周期，后端最小限制 2000 ms |
| `OPTION_WORKSTATION_AUDIT_PATH` | `./.option-workstation/audit.jsonl` | 本地研究审计账本 |

## 实时数据流

```text
Schwab OAuth
    |
    v
Access / Refresh Token in Rust memory
    |
    v
Schwab Market Data REST
    |-- quotes
    |-- expirationchain
    |-- chains
    `-- pricehistory
    |
    v
Schwab parser / quality guard
    |
    v
Rust analytics
    |-- BSM / Greeks
    |-- GEX / Vanna / Charm
    |-- SVI / Surface
    `-- Strategy risk
    |
    v
/api/live/snapshot
/api/live/stream (local WebSocket)
    |
    v
React workstation
```

## 并发与刷新保护

实时 session 切换时，后台旧请求可能比新请求更晚返回。本 fork 在提交 refresh 结果前会再次核对 active universe。

如果用户已经切换标的、到期日、配置或断开连接，旧 refresh 的 snapshot 和 error 都不会覆盖当前 session。

OAuth token refresh 也有互斥保护，避免多个并发请求同时刷新同一个 token。

## API 研究接口

常用本地接口包括：

```text
GET    /api/health
GET    /api/connection
POST   /api/connection
DELETE /api/connection
GET    /api/oauth/status
POST   /api/oauth/start
POST   /api/live/session
GET    /api/live/snapshot
GET    /api/live/volatility-context
GET    /api/live/stream
POST   /api/strategy/analyze
GET    /api/audit/records
POST   /api/audit/records
```

交易账户和订单 API 不在当前 Router 中。

## CI / 验证

仓库 GitHub Actions 会执行：

```text
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
npm ci
npm run build
npm audit --audit-level=high
Python reference tests
License metadata checks
Publication / secret checks
```

对 Schwab 适配新增了针对以下边界的 Rust 回归测试：

- Schwab IV 百分比归一化；
- provider Greek sentinel 过滤；
- standard / mini / non-standard contract 选择；
- delayed / truncated / underlying timestamp 解析；
- 最新可用分钟 session；
- cumulative VWAP proxy；
- OAuth callback URL 解码；
- Schwab symbol normalization；
- curl stdin config escaping。

## 已知边界

- Schwab Market Data 权限和实时性由用户账号及 Schwab entitlement 决定；
- REST 刷新不等同于交易所级 streaming feed；
- OI 通常是快照型元数据，不能理解为逐笔实时持仓变化；
- BSM 对美式个股期权属于模型近似；
- Dealer sign 是研究假设，不代表已知做市商真实库存；
- SVI / constrained surface 是研究工具，不构成严格的无套利成交证明；
- 当前 OAuth Callback 采用复制最终 callback URL 回工作台完成 token exchange，还没有内置本地 HTTPS callback listener；
- UI 的常规股票代码输入路径仍以美股 ticker 为主要使用场景，特殊指数 / 类股代码可受前端输入校验限制。

## 安全建议

- 默认保持 `OPTION_WORKSTATION_HOST=127.0.0.1`；
- 不要把 `.env`、App Secret、Access Token、Refresh Token 提交到 Git；
- 不要把本地服务直接暴露到公网；
- Callback URL 与 Schwab Developer Portal 配置保持完全一致；
- 只申请实际需要的 Schwab API 产品。本 fork 只需要 Market Data；
- 发布前运行仓库自带的 publication checks。

## License

Apache-2.0，见 [LICENSE](LICENSE)。

上游项目：`khakhasshi/OptionWorkstation`。
