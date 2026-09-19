# Agent contract

- Start with `family_agent_context` (or its bounded service-core equivalent)
  to establish the repository manifest, current `mncs-language` identity, and
  Commons architecture identity before broad repository search.
- Query the authoritative language capability index before adding host code.
  A missing generic language/stdlib/runtime facility is a pressure to the
  owning repository, not permission to create a semantic Rust workaround.
- Language Service owns bounded query/protocol projection and resident
  orchestration; `mncs-language` owns language semantics, and Commons owns
  family architecture and pressure lifecycle.
- Keep Rust to protocol, transport, cache, and other explicit boundaries.
  Do not make a host implementation a silent semantic authority or create a
  competing family registry.
- Query Commons for an existing pressure before recording a new one. Preserve
  `UNKNOWN` and incomplete context rather than inferring sufficiency.
