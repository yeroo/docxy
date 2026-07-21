# Aliaksei VDI — context map (real-world source, graph LR)

Copied verbatim from `2026-07-20-vdi-domain-model-design.md` §6 "Context map" — exercises heavy fan-out edges (`A -->|label| B & C & D & ...`).

```mermaid
graph LR
  IA[#11 Identity & Access<br/>OHS + Published Language: JWT claims]
  DI[#12 Desktop Identity<br/>ACL over AD/Entra/Okta]
  OE[#6 Org & Entitlement]
  IMG[#4 Imaging & Promotion<br/>Published Language: image registry]
  POOL[#2 Machine Pools]
  DL[#1 Desktop Lifecycle & Sessions<br/>core event source]
  DCV[dcv-gateway edge]
  PV[#3 Profile Persistence & Backup]
  CAT[#7 App Catalog & Launch]
  TKT[#8 Ticketing & Support<br/>OHS + Published Language: event backbone]
  OBS[#9 Observability & Alarms]
  REP[#10 Reporting]
  AI[#5 AI / Conversational Interface<br/>consumes every context via MCP]
  NB[#13 File / Notebook Storage]

  IA -->|claims| DL & POOL & OE & CAT & TKT & OBS & REP & PV & DI & NB & AI
  OE -->|scope, entitlements| POOL & DL & CAT
  IMG -->|OSImagePromoted| POOL
  IMG -->|AppImagePromoted| CAT
  POOL -->|SKU/AMI/region/cap| DL
  DI -.->|DesktopIdentityReady| DL
  DL -->|vm lifecycle events| PV & OBS & AI
  DL <-->|session resolve| DCV
  CAT -->|install / launch| DL
  OBS -->|AlarmStateChanged → smart ticket| TKT
  TKT -->|ticket events / approvals| CAT & OBS & AI
  REP -.->|reads projections| DL & OE & POOL & TKT & OBS
  AI -->|MCP tools| DL & POOL & OE & CAT & TKT & IMG & OBS
```
