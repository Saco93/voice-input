# Contributing

Voice Input is a Rust 2024 application with two Quickshell/Qt Quick interfaces:
the runtime HUD and the settings window. Changes should preserve low capture
latency, bounded resource use, private credential handling, and compatibility
with Qt's cross-platform Rendering Hardware Interface (RHI).

## Engineering baseline

The project follows these primary references:

- [The Rust Style Guide](https://doc.rust-lang.org/style-guide/) through
  `rustfmt`.
- [The Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) for
  public interfaces where they apply to this binary project.
- [Clippy](https://doc.rust-lang.org/clippy/) with all default lints treated as
  errors in CI. Pedantic and nursery lints are evaluated individually; they are
  not blanket requirements.
- [The Rust Book: Error Handling](https://doc.rust-lang.org/book/ch09-00-error-handling.html)
  and the [Rust Error Handling Project Group](https://rust-lang.github.io/project-error-handling/).
- [Qt QML Coding Conventions](https://doc.qt.io/qt-6/qml-codingconventions.html),
  [Qt Quick Best Practices](https://doc.qt.io/qt-6/qtquick-bestpractices.html),
  and [Qt Quick Performance](https://doc.qt.io/qt-6/qtquick-performance.html).
- [Qt ShaderEffect](https://doc.qt.io/qt-6/qml-qtquick-shadereffect.html) and
  [Qt Shader Tools](https://doc.qt.io/qt-6/qtshadertools-build.html) for shader
  assets.
- The [Quickshell guide](https://quickshell.org/docs/v0.3.0/guide/introduction/)
  and its API documentation. Quickshell does not currently define a separate,
  comprehensive style standard, so Qt's QML guidance is the default.

Community references such as [Rust Design Patterns](https://rust-unofficial.github.io/patterns/)
may inform a change, but a pattern should be introduced only when it simplifies
the current design or enforces an invariant.

## Design rules

### Rust

- Keep fallible operations in `Result`; add context at I/O, process, protocol,
  and service boundaries. User-facing errors must not contain credentials,
  complete provider responses, or arbitrary clipboard contents.
- Bound every external input, queue, response body, recording, and wait. Define
  explicit behavior for timeout, disconnect, backpressure, and partial data.
- Use domain types and enums for closed sets of states. Validate data whenever
  it crosses from TOML, JSON, a socket, a subprocess, or a remote service into
  trusted application state.
- Keep `unsafe` code minimal. Every block requires a nearby `SAFETY` comment
  that states the invariant relied upon.
- Avoid holding a mutex while performing unbounded I/O or waiting for an
  unrelated worker. If serialization is intentional, document that invariant
  and ensure every wait has a deadline.
- Prefer focused modules and pure helpers when they make lifecycle behavior
  testable. Do not split modules solely to reduce line counts.

### QML and Quickshell

- Use explicit `id`, `required property`, concrete property types, and typed
  function signatures when the imported QML type metadata supports them.
  Reserve `var` for JSON-like protocol values and genuinely heterogeneous data.
- Keep shared `Process`, `Socket`, and `Timer` objects outside per-screen
  `Variants`. Components created once per screen must contain presentation-only
  state whenever possible.
- Keep bindings declarative and inexpensive. Per-frame handlers should update
  animation scalars rather than parse strings, allocate arrays, or perform I/O.
- Validate JSON and socket frames before assigning them to properties. Preserve
  the last valid state after malformed or partial input, and report failures
  without logging secrets.
- Use argument arrays for `Process.command`; do not pass user data through a
  shell. Requests need a timeout and a recoverable process lifecycle.
- Compile shaders to `.qsb`. Maintain the Qt 6 uniform block and binding rules,
  premultiplied alpha, and all CI-validated GLSL/HLSL/MSL targets.

## Validation

Run the standard checks before submitting a change:

```sh
make validate
qmllint -I /usr/lib/qt6/qml assets/quickshell/*.qml
qmllint -I /usr/lib/qt6/qml assets/quickshell-settings/*.qml
make hud-shaders QSB=/usr/lib/qt6/bin/qsb
git diff --check
```

`qmllint` must run with the Qt and Quickshell import paths from a complete local
installation. Unresolved imports make a lint result incomplete and must not be
silently ignored.

Tests should cover normal behavior and the relevant boundary: malformed input,
maximum size, timeout, disconnect, stale state, or concurrent access. Prefer a
small pure unit test for parsing and invariants, then add an integration test
when correctness depends on a socket, process, filesystem permission, or
multi-threaded lifecycle.

## Commit scope

Keep functional fixes, mechanical formatting, dependency updates, and large UI
component moves in separate commits when practical. A refactor should preserve
observable behavior and should be followed by a focused behavior change rather
than mixed with it.
