# Security Policy

The MNCS Language Service has a working read-only core with LSP and MCP adapters but remains experimental and is not production-ready. Both adapters are local stdio servers; no network transports are implemented.

## Security-sensitive areas

As implementation develops, treat the following as security-sensitive boundaries:

- workspace file access and path handling;
- protocol client authentication/authorization where network transports are introduced;
- MCP and future MNCS-native tool invocation;
- semantic mutation/refactoring requests;
- execution of compiler, verifier, backend, Forge, or Fabric work;
- stale snapshot and identity confusion;
- capability/authority information exposed across client boundaries;
- untrusted agent-generated candidate changes;
- persistent caches and durable evidence publication.

## Design posture

The service should fail closed when subject identity, snapshot identity, authority, or evidence state is ambiguous.

External clients and agents must not gain semantic authority merely because they can connect to the service. Language authority, capability, verification, evidence, and promotion rules remain governed by authoritative MNCS components.

The service should not execute arbitrary commands as an incidental consequence of editor or semantic-query operations.

## Reporting

Please report suspected vulnerabilities privately through GitHub's security reporting facilities when available rather than opening a public issue containing exploit details.

Because the project is pre-production, compatibility and security policies may change rapidly; security-relevant behavior should not be assumed stable until explicitly documented as such.
