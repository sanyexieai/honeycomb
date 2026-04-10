# Workspace Data

This directory contains Markdown-first operational data used by Honeycomb.

Recommended layout:

```text
workspace/
  tenants/
    <tenant-id>/
      users/
        <user-id>/
          personas/
          capabilities/
          memory/
          sessions/
          instances/
          decisions/
          indexes/
```

Default local development may use `workspace/tenants/local/users/default/`.
