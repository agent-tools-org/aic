---
id: no-unwrap-in-prod
severity: advisory
scope: ['src/**/*.rs']
layer: l2
---
# No unwrap() in production code

Flag uses of .unwrap() outside test modules (#[cfg(test)] or tests/). Prefer .expect() with a clear message, or ? with anyhow/Result propagation.

Ask the model: Does the diff add any .unwrap() calls outside of test code? List each occurrence with file:line and suggest the preferred alternative.

Pass if no new unwraps in non-test code. Fail otherwise.
