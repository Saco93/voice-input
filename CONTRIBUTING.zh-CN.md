# 贡献指南

[English](CONTRIBUTING.md)

Voice Input 是一个采用 Rust 2024 的应用，并包含两套 Quickshell/Qt Quick 界面：运行时 HUD 和 Settings 窗口。修改时应继续保证较低的采集延迟、受限的资源使用、私密的 credential 处理，以及与 Qt 跨平台渲染硬件接口（Rendering Hardware Interface，RHI）的兼容性。

## 工程基线

项目主要遵循以下参考资料：

- 使用 `rustfmt` 应用[《Rust Style Guide》](https://doc.rust-lang.org/style-guide/)的规范。
- 本项目是 binary application；其中适用的 public API 参考[《Rust API Guidelines》](https://rust-lang.github.io/api-guidelines/)。
- CI 会运行 [Clippy](https://doc.rust-lang.org/clippy/)，并把全部默认 lint 视为错误。Pedantic 和 nursery lint 会逐项评估，不作为整体强制要求。
- 错误处理参考[《The Rust Book: Error Handling》](https://doc.rust-lang.org/book/ch09-00-error-handling.html)和[《Rust Error Handling Project Group》](https://rust-lang.github.io/project-error-handling/)。
- QML 参考[《Qt QML Coding Conventions》](https://doc.qt.io/qt-6/qml-codingconventions.html)、[《Qt Quick Best Practices》](https://doc.qt.io/qt-6/qtquick-bestpractices.html)和[《Qt Quick Performance》](https://doc.qt.io/qt-6/qtquick-performance.html)。
- Shader asset 参考[《Qt ShaderEffect》](https://doc.qt.io/qt-6/qml-qtquick-shadereffect.html)和[《Qt Shader Tools》](https://doc.qt.io/qt-6/qtshadertools-build.html)。
- Quickshell 参考其[使用指南](https://quickshell.org/docs/v0.3.0/guide/introduction/)和 API 文档。Quickshell 目前没有单独且完整的样式标准，因此默认采用 Qt 的 QML 指南。

[《Rust Design Patterns》](https://rust-unofficial.github.io/patterns/)等社区资料可以作为参考，但只有在某种模式能够简化当前设计或明确保证不变量时，才应引入该模式。

## 设计规则

### Rust

- 可失败的操作应使用 `Result`。在 I/O、process、protocol 和 service 边界补充上下文。面向用户的错误不得包含 credential、完整的 provider response 或任意 clipboard 内容。
- 所有外部输入、queue、response body、录音和等待都必须设置上限。必须明确 timeout、断开连接、backpressure 和部分数据的处理行为。
- 对封闭状态集合使用 domain type 和 enum。TOML、JSON、socket、subprocess 或远程 service 的数据进入可信应用状态时必须经过验证。
- 尽量减少 `unsafe`。每个 `unsafe` block 附近都必须有 `SAFETY` 注释，用来说明它依赖的不变量。
- 不要在执行无上限 I/O 或等待无关 worker 时持有 mutex。如果有意进行串行处理，应记录该不变量，并确保每次等待都有 deadline。
- 如果拆分为职责明确的 module 和纯 helper 可以测试生命周期行为，应优先采用这种结构。不要只为减少文件行数而拆分 module。

### QML 与 Quickshell

- 在导入的 QML type metadata 支持时，使用明确的 `id`、`required property`、具体 property type 和带类型的 function signature。只有 JSON-like protocol value 或确实包含不同类型的数据才使用 `var`。
- 共享的 `Process`、`Socket` 和 `Timer` 应位于按 screen 创建的 `Variants` 之外。每个 screen 创建一次的 component 应尽量只保存 presentation state。
- Binding 应保持声明式且执行成本较低。每帧 handler 应更新 animation scalar，不应解析 string、分配 array 或执行 I/O。
- JSON 和 socket frame 必须先验证，再赋给 property。输入损坏或不完整时应保留最近一次有效状态，并在不记录 secret 的前提下报告错误。
- `Process.command` 应使用 argument array，不要让用户数据经过 shell。Request 必须有 timeout，并提供可恢复的 process lifecycle。
- Shader 应编译为 `.qsb`。必须保留 Qt 6 uniform block 和 binding 规则、premultiplied alpha，以及 CI 验证的全部 GLSL/HLSL/MSL target。

## 验证

提交修改前运行标准检查：

```sh
make validate
make hud-shaders QSB=/usr/lib/qt6/bin/qsb
git diff --check
```

`make validate` 会使用当前 Qt 和 Quickshell 安装提供的 import path 运行 `qmllint`。如果 executable 不在 `PATH` 中，请设置 `QMLLINT`。存在未解析 import 时，lint 结果并不完整，不应忽略该问题。

测试应覆盖正常行为和相关边界，包括损坏的输入、最大尺寸、timeout、断开连接、过期状态或并发访问。解析和不变量优先使用较小的纯 unit test；如果正确性取决于 socket、process、filesystem permission 或多 thread lifecycle，再增加 integration test。

## 提交范围

在可行的情况下，把功能修复、机械式格式调整、dependency 更新和大规模 UI component 移动放入不同提交。Refactor 应保持可观察行为不变，后续行为修改应使用单独且范围明确的提交。
