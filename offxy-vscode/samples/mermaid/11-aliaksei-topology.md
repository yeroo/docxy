# Aliaksei VDI — deployment topology (real-world source, graph TB)

Copied verbatim from `2026-07-20-vdi-domain-model-design.md` §7 "Deployment topology" — exercises nested subgraphs plus a `{{hexagon}}` shape node.

```mermaid
graph TB
  subgraph SharedVPC[Shared VPC · per Customer]
    UI[Web UI + dev-portal]
    AI[#5 AI / MCP]
    IA[#11 Identity & Access]
    OE[#6 Org & Entitlement]
    CAT[#7 App Catalog]
    TKT[#8 Ticketing]
    REP[#10 Reporting]
    NB[#13 Notebook]
    DLc[#1 Desktop Lifecycle · control + projection]
    POOLc[#2 Machine Pools · definition/quota]
    IMGc[#4 Imaging · registry/promotion]
    OBSc[#9 Observability · config/alarms]
  end
  subgraph WorkloadVPC[Workload VPC · per region]
    DLe[#1 Desktop effector · EC2/SSM/DCV]
    DCV[dcv-gateway]
    POOLe[#2 Pool capacity effector]
    PV[#3 Profile Persistence effector · EBS/FSx]
    DI[#12 Desktop Identity effector · AD/Entra]
    IMGe[#4 Image build pipeline]
    OBSe[#9 Metric collectors · CloudWatch]
  end
  EB{{EventBridge bus<br/>cross-account / cross-region<br/>+ Schema Registry}}
  SharedVPC <-->|command & fact events| EB
  EB <-->|command & fact events| WorkloadVPC
```
