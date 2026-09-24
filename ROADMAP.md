# Roadmap

This roadmap describes direction, not a delivery promise.

## 0.1 Public Preview

- reproducible source and container builds;
- replay and live quality gates;
- BSM/SVI/surface/exposure analytics;
- guarded paper-account monitoring and submission;
- beginner guide, architecture, data contract, and release process.

## 0.2 Reliability

- [x] versioned replay snapshot contract and API documentation;
- [x] replay request generation guards and live feed observability;
- synthetic, redistributable replay fixture for end-to-end CI;
- deterministic live-event recorder and offline playback;
- stronger audit-ledger export and verification tooling;
- structured benchmark suite for chain and surface latency;
- split the charting and WebGL bundles to reduce first-load transfer size;
- remove the local Longbridge OAuth patch after an equivalent secure upstream
  SDK release is available.

## 0.3 Research Workflow

- [x] point-in-time strategy backtest MVP with delta-based contract selection and executable-side pricing;
- [x] first/second-order P/L attribution with explicit unexplained residual;
- [x] audit-backed trade journal replay;
- [x] strategy regime scanner for IV, GEX sign, and gamma-flip context;
- [x] named local research workspaces with restore/export state;
- [x] executable quote spread and minimum-leg-quality diagnostics;
- richer cross-expiry and scenario diagnostics;
- reproducible strategy templates with assumption manifests;
- research result export without provider-owned raw data.

## Before 1.0

- threat-model review by an independent contributor;
- documented compatibility and deprecation policy;
- atomic broker complex-order support or explicit permanent exclusion;
- broader accessibility and internationalization review;
- release provenance and signed artifacts.

Real-money automated trading is not on the roadmap.


## Research Lab v1 follow-ups

The first research-lab implementation deliberately keeps the strategy language
small. The next reliability work should add frozen strategy manifests,
commission/slippage models, stop/target exits, rolling rules, walk-forward
splits, and portfolio-level capital accounting before broader strategy
templates or UI automation.
