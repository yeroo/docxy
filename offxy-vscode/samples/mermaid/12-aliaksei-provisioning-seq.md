# Aliaksei VDI — provisioning choreography (real-world source, sequenceDiagram)

Copied verbatim from `2026-07-20-vdi-domain-model-design.md` §8 "Flagship choreography — provisioning" — exercises the sequence-diagram best-effort renderer.

```mermaid
sequenceDiagram
  participant U as User / AI (Shared)
  participant DL as Desktop Lifecycle #1 (Shared)
  participant EB as EventBridge
  participant DI as Desktop Identity #12 (Workload)
  participant DLe as Desktop effector #1 (Workload)
  participant PV as Profile Persistence #3 (Workload)
  U->>DL: request Machine (pool P, region R)
  DL->>DL: check privilege + pool MAX
  DL->>EB: MachineProvisionRequested (customer, workload=R, subjectRef)
  EB->>DI: (route to region R)
  alt automated mode (platform has rights)
    DI->>DI: provider acts (AD join+account / Entra device-join / none)
  else manual mode (no rights → human performs)
    DI->>DI: raise human task (via Ticketing #8), then wait for completion
  end
  DI->>EB: DesktopIdentityReady
  EB->>DLe: MachineProvisionRequested + identity ready
  DLe->>DLe: create EC2 from pool AMI/SKU, SSM bootstrap, DCV
  DLe->>EB: MachineProvisioned
  EB->>PV: MachineProvisioned
  PV->>PV: attach/prepare Profile Volume
  PV->>EB: ProfileVolumeReady
  EB->>DL: update projection → notify UI/AI
```
