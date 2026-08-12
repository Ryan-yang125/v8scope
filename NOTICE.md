# Notices

V8Scope was designed after studying the following open-source projects:

- Clinic.js (`node-clinic`, `node-clinic-doctor`, `node-clinic-flame`, `node-clinic-bubbleprof`, and `node-clinic-heap-profiler`), copyright NearForm and Clinic.js contributors, MIT License. Its executable tests form the traceability baseline, and its Doctor CPU analysis informed the deterministic Rust HMM implementation. The complete upstream permission notice is distributed in `licenses/CLINIC-MIT.txt`.
- agent-browser, copyright its contributors, Apache License 2.0. Its native Rust CDP client informed the request routing, cancellation cleanup, timeout, disconnect, and keepalive design.
- inferno 0.12.8, copyright its contributors, CDDL-1.0. It renders the offline CPU flame graph. The complete license is distributed in `licenses/INFERNO-CDDL-1.0.txt`; corresponding source is available from <https://crates.io/crates/inferno/0.12.8> and <https://github.com/jonhoo/inferno/tree/v0.12.8>.

Release SBOMs contain the complete dependency inventory and license metadata.
