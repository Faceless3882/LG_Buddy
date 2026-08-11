# Native webOS Testing

This document describes how LG Buddy tests its native webOS boundary.

It is implementation guidance, not an attempt to define a general webOS
emulator. The goal is to provide a reliable TV boundary for LG Buddy while
keeping every claimed device behavior traceable to real hardware evidence.

## Confidence Chain

```mermaid
flowchart LR
    TV["Real TV probe"] --> Evidence["Recorded request, response, and state"]
    Evidence --> Characterization["Evidence-linked behavior test"]
    Evidence --> Mock["Central stateful webOS test server"]
    Characterization -->|exercises| Mock
    Mock --> Feature["LG Buddy functionality tests"]
```

Each step has a distinct responsibility:

- real-TV probes establish what the device actually does
- issue comments preserve the relevant request, response, starting state, and
  resulting state
- characterization tests state the behavior LG Buddy relies on and link to the
  evidence
- the stateful server centralizes complete webOS frames and TV transitions
- higher-level tests use that server to exercise LG Buddy behavior without
  reproducing webOS payloads

`bscpylgtv` remains useful as a source of hypotheses and compatibility clues.
It is not evidence that the native client or mock should copy without a real-TV
observation.

## Test Surfaces

### Central stateful test server

`crates/lg-buddy/src/web_os/test_support/test_server.rs` is the only webOS
server exposed to tests. It owns registration, complete response frames,
permission checks, state transitions, TLS setup, and named protocol-fault
scenarios.

Its stateful TV behavior includes:

- changing the foreground application after an input switch
- changing picture backlight after an authorized write and exposing the numeric
  result through a later read
- moving between `Active` and `Screen Off`
- moving to `Power Off` and rejecting an immediate new registration

The server validates the request URI and payload sent by the client before it
responds. Controls mutate server state, and later reads expose that state. This
lets tests verify an operation through an independent observation instead of
only accepting its immediate acknowledgement.

Tests select semantic scenarios such as registration rejection, response
timeout, malformed frame, or permission denial. They cannot author raw server
frames. Successful responses, rejected operations, accepted operations with no
state change, and transport failures therefore have the same response owner.

Whether a behavior was observed on hardware or introduced as defensive fault
injection is documented by the test and its evidence link. Provenance does not
change which server constructs the response.

The server should fail when a test asks for an unmodeled URI, input, or state
combination. Unsupported behavior is intentionally visible;
the mock must not invent a plausible response to make a test pass.

Complete webOS response JSON belongs in this server. Tests may assert domain
values and error details that matter to LG Buddy, but must not create another
server or peer that carries response fixtures.

### Characterization tests

`crates/lg-buddy/src/web_os/observed_behavior.rs` exercises the real native
client against the stateful server. These tests define the device contract that
LG Buddy currently relies on.

Each observed behavior test must include a comment linking to the issue or
issue comment that contains its hardware evidence. A useful test states:

- the initial TV state
- the native operation being performed
- the returned domain result or exact meaningful error
- the resulting state, preferably verified by a later read

Registration behavior may be characterized beside the authentication code when
that keeps token persistence and authentication events in the same test. It
must still use the centralized test server and link to its evidence.

### Protocol and fault scenarios

The central server also owns named synthetic scenarios for low-level protocol
mechanics, including:

- malformed JSON frames
- unrelated or incorrect response IDs
- timeouts and early connection closure
- deliberately rejected registration
- responses that contradict observed token behavior

These scenarios exercise the same client boundary without exposing a raw peer
or creating another place where webOS response frames can be assembled.

Pure parser tests remain appropriate for malformed, boundary, and forward-
compatibility values. They do not emulate a TV or define a reusable response
fixture.

### LG Buddy functionality tests

Tests above the webOS module should use the stateful server when native TV
behavior is part of the scenario. These tests should focus on LG Buddy outcomes:
policy decisions, retries, state ownership, command results, and user-visible
behavior. They should not know the normal webOS wire response shape.

The profile-bound native adapter uses the same server to cover the complete
`TvClient` contract, session reuse, transport invalidation, later reconnection,
and ambiguous-write no-replay behavior. The server can also support higher-level
tests of `TvDevice`, command orchestration, and acceptance scenarios. The
characterization suite remains responsible for documenting and guarding the
observed device contract represented by the server.

A passing mock-backed functionality test proves LG Buddy behavior against the
current device model. It is not evidence that an unverified device behavior is
real.

## Adding Or Refining Behavior

Use this sequence when implementing another native webOS operation or learning
about a device quirk:

1. Drive the operation manually against the real TV using the temporary
   `lg-buddy dev webos-auth-probe`, `lg-buddy dev webos-read-probe`, or
   `lg-buddy dev webos-control-probe <operation>` surface, or an equally narrow
   diagnostic.
2. Record the initial state, request, response, and resulting state in the
   relevant GitHub issue. Remove access tokens and other secrets.
3. Add or update an evidence-linked characterization test.
4. Extend the centralized stateful server with the observed response and state
   transition. Preserve exact quirks when LG Buddy behavior depends on them.
5. Add named server scenarios or parser tests for defensive behavior that
   cannot be claimed as an observation.
6. Use the refined server in the LG Buddy functionality tests for the new
   operation.
7. Run the focused webOS tests, then the normal workspace suite.

```bash
cargo test -p lg-buddy --lib web_os
cargo test --workspace
```

If later evidence contradicts the current model, update the evidence-linked
test and server together. Downstream functionality tests should inherit the
refined behavior without changing their own webOS fixtures.

## Transport Boundary

The test server supports local plain WebSocket transport for most behavior
tests and WSS transport for the self-signed certificate test. Both transports
use the same server and response construction.

Keeping transport assertions and device-semantics assertions in distinct tests
makes failures diagnostic: a characterization failure points to device
semantics, while a WSS test failure points to transport setup.

## Review Checklist

For native webOS test changes, check that:

- every claimed device behavior has a hardware evidence link
- complete response fixtures have one home in the stateful server
- controls update state and are verified through a meaningful read when
  possible
- unobserved behavior fails clearly instead of being generalized
- protocol faults are selected through named scenarios on the same server
- higher-level LG Buddy tests assert domain outcomes rather than wire JSON
- access tokens and device-specific secrets are absent from fixtures and logs
