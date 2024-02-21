# Rust Implementation of JSON-RPC 2.0

This crate contains types and interfaces for creating clients and servers that are compliant with the [JSON-RPC 2.0 Specification](https://www.jsonrpc.org/specification)

## Spec Compliance

The [Conventions](https://www.jsonrpc.org/specification#conventions) section of the JSON-RPC 2.0 Specification defines some key words that are used to express required, allowed, and disallowed behavior. The words that are actually used in the spec are:

- "MUST" (used 16 times)
- "MUST NOT" (used 7 times)
- "REQUIRED" (used 3 times)
- "SHOULD" (used 4 times)
- "SHOULD NOT" (used 2 times)
- "MAY" (used 5 times)

The types and interfaces provided by this crate enforce most of the rules defined by these key words, but there are a few that cannot be enforced by this crate and requires correct implementation on behalf of anyone building custom transports, handlers, or clients in order to achieve compliance with the JSON-RPC 2.0 Specification.

TODO: Cover all of the uses of key words that are not enforced by the crate and require user-level correctness.
