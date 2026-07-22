# Basic flow — elbow connectors

```mermaid
flowchart TD
  A[Start] --> B{Approved?}
  B -->|yes| C[Provision]
  B -->|no| D[Reject]
  C --> E[Notify user]
  D --> E
```
