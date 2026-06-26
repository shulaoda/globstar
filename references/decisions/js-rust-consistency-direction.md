# globstar：JS 侧高效实现 + JS↔Rust 一致性 —— 研究与方向建议

> 研究方法：读完两侧全部源码（Rust ~4500 行 / JS ~3700 行），**实跑**了两边的差分语料；再用一个多 agent 工作流（14 个 agent，含真实原型：编译了一个 wasm32 版本并测速、把 picomatch 跑过整个语料、写了一个 100 万例的差分 fuzzer、写了 glob→正则翻译器并对全语料验证）做了 5 条路线的对抗式评估。下面是结论。

---

## 0. 一句话结论

- **picomatch 不能用来保证一致**：实测它和 globstar 的 spec 在 **126/3322 条语料（3.8%）** 上结果相反，分 8 类结构性差异，且无法靠选项修掉。用了它就等于把"dev/build 不一致" bug 请回来。
- **现在两侧其实是一致的**（语料全过、零失败），你担心的"不一致"是**潜在漂移（latent drift）**——两份手写引擎在语料没覆盖的输入上可能悄悄不一样，而**目前根本没有 fuzzer / 没有 Rust 命令行入口去测它**。
- **最关键的真相**：现在 rolldown（build 侧）用的是 `fast-glob`，Vite dev 侧用的是 `picomatch`，**两边都没在用 globstar**。所以"JS==Rust 内部一致"只是必要条件，不是充分条件。
- **推荐**：分两步走。**第 0 步（现在就做）= 方向 D**：写一个 Rust stdin 差分入口 + JS 随机差分 fuzzer，第一次真正"量"出漂移。**第 1 步（终态）= 方向 A 的改良版**：用 **native N-API（而不是 wasm）** 把同一份 Rust crate 同时喂给 Node 和 rolldown —— 这是唯一能让漂移"从结构上变成不可能"的方案，但要等 fuzzer 全绿 + 至少一个消费方愿意接入。

---

## 1. 现状：当前实现到底是什么

globstar 是给 Vite `import.meta.glob` 专门做的 glob 库，**Rust crate 是权威实现，JS 是手写移植**，靠一份差分语料（`crates/globstar/tests/corpus/*.txt`）对齐。

**处理流水线（两种语言各手写了一份，语义必须一模一样）：**

1. `parse`：pattern 字节 → AST（parser.rs 340 行 / parser.js 266 行）。处理 `!` 取反、`/`、`?`、`*`、`**`（必须独占一段否则退化成 `*`）、`[...]` 字符类（POSIX 首个 `]` 规则、范围、转义、类里 `/` 报错）、`{...}` 花括号（不预展开、嵌套≤32、单分支 `{a}` 当字面量）、`\` 宽松转义。
2. `lower`：AST → 线性指令程序（ops.rs 423 / ops.js 260）。把 `**` 折叠成专用 op（`**/`→OptSegmentsSlash、`/**`→SlashAnything、`**`→GlobstarAny…），抽取 `LiteralFacts` 后缀预过滤 + 静态前缀（walker 直跳目录）。
3. 选引擎：纯字面量→LiteralMatcher（字节比较）；否则 PikeVm（默认，编译便宜）或 ThompsonDfa（walker 用，超状态上限退回 PikeVm）。多模式并集会做前后缀因子合并，NFA>64 状态就拆成 per-pattern 的 OrEngine。
4. `match`：引擎在 **UTF-8 字节**上跑。分隔符集 `Seps = {/}`，Windows 上加 `\`。整路径匹配。`dot`（matcher 默认 true / walker 默认 false）= bash 隐藏文件保护。`case_insensitive` = **仅 ASCII** 折叠。
5. `match_dir`：返回 Pruned/Descend/Match/DescendAndMatch 四值，给 walker 剪枝。

**实测现状（我亲自跑的）：**

- `cargo test --test corpus`：Rust 通过 单 3322 + 多 62 + 目录 66 + 错误 28，**0 失败**。
- `node verify.mjs --skip-rust`：JS 三个引擎（公开 API / 强制 DFA / 强制 PikeVm）通过 单 3322 + 多 62 + 错误 28，**0 失败**。
- 性能（BENCHMARKS.md）：Rust globstar 是所有库里最快的（每次匹配 2–7ns）。**JS globstar 单次匹配比 picomatch 慢**（globstar 模式 66ns vs picomatch 42ns；brace-suffix 180 vs 58），因为 picomatch 编译成**原生 RegExp**（V8 的 C++ JIT）。**但 JS globstar 在端到端 walker 上反而比 fast-glob/tinyglobby 快**。

> ⚠️ 一个我独立发现、很关键的点：JS globstar 在 walker 上赢，**不是因为引擎快，而是因为** `facts` 后缀预过滤 + `match_dir` 四值剪枝 + 静态前缀直跳目录。**纯 RegExp 只能回答"匹配/不匹配"，给不了 `match_dir` 的"这个目录还能不能往下走"**。所以任何"改用正则"的方案，都没法替掉 walker 的剪枝引擎。

---

## 2. 真正的问题（不是你以为的那个 bug）

你说的"JS 侧和 Rust 侧 syntax 不一致"，实测下来**不是现在语料里能看到的 bug**，而是两个更深的结构性问题：

### 问题 A：潜在漂移（latent drift），而且现在测不出来

两份各约 4000 行的正则引擎，只靠一份**手写固定语料**对齐。语料没覆盖的输入空间是**无限**的，两边可能悄悄不一致。而且：

- **没有任何 property-based / 随机差分测试**。
- **没有 Rust 的命令行/stdin 入口**可以喂任意输入对比 —— 所以"JS vs Rust 漂移"这件事**今天根本无法测量**。
- 语料的薄弱区（agent 实测统计）：非 ASCII 字节只有 53 行（1.6%）；`dot=false`（walker 默认模式）只有 70 行 vs `dot=true` 的 3252 行；Windows 反斜杠分隔符在 darwin/linux CI 上**完全没测**；≥6 分支的大花括号并集只有 7 行。
- 证据：C2 agent 写的 glob→正则翻译器，一个"简单 dot 守卫"的 bug **通过了全部 3322 条语料**，却在 100 万随机例里被抓到 13 例漂移。**这正说明固定语料兜不住。**

### 问题 B：双重维护

spec 还在变（README：每个 0.0.x 都可能破坏性改动）。每改一次语义要在两种语言里各改一遍 4000 行 —— 这正是漂移的来源。

### 真相 C：globstar 现在谁都没用（最重要的战略事实）

- **rolldown** 的原生 import-glob 插件依赖 `fast-glob`（`crates/rolldown_plugin_vite_import_glob/Cargo.toml`），用 `fast_glob::glob_match` 匹配、`walkdir` 遍历、`.to_lowercase()` 模拟大小写不敏感（源码注释自己承认"会在 `[A-Z]` 范围和非 ASCII 上发散"）。全仓库 `grep globstar` = 0 命中。
- **Vite dev** 用 `picomatch ^4` + `tinyglobby`（`importMetaGlob.ts`）。
- 所以"dev 匹配到、build 没匹配到"这个 bug **现在就在 picomatch(dev) 和 fast-glob(build) 之间真实存在**，而 globstar 故意和这两者都不一样（仓库里甚至有 `corpus-fast-glob-diff.txt` 记录和 fast-glob 的有意差异）。

**含义**：让 globstar 的 JS 和 Rust 内部一致，是**必要但不充分**的。要真正实现"dev==build"，**两个消费方都得换成 globstar**。这点决定了后面所有方向的实际价值。

---

## 3. picomatch 能不能保证一致？（你点名要探索的方向，实测答案：不能）

agent 把 picomatch v4.0.4 跑过了**整个单模式语料**：和 spec 期望值**冲突 126/3322（3.8%）**，随机 20 万例里也是 ~2.1%。8 类结构性差异，且**都是 picomatch 正则代码生成里写死的，`{dot, nocase}` 选项改不掉**：

| #   | 差异                                              | 例子                         | picomatch | globstar       |
| --- | ------------------------------------------------- | ---------------------------- | --------- | -------------- |
| 1   | 忽略 POSIX `[!..]` 取反（53 行！）                | `[!abc]` vs `d`              | no-match  | match          |
| 2   | `*`/`**`/`{a,}` 不匹配空路径（21 行）             | `*` vs `""`                  | no-match  | match          |
| 3   | 吃掉字母数字前的转义                              | `\b` vs `b`                  | no-match  | match          |
| 4   | 把 `()` 当正则分组/extglob                        | `file(1).txt` vs `file1.txt` | match     | no-match       |
| 5   | `a/**` 匹配目录 `a` 本身（spec §16 故意禁止）     | `a/**` vs `a`                | match     | no-match       |
| 6   | 尾斜杠路径行为相反                                | `*` vs `a/`                  | match     | no-match       |
| 7   | 大小写折叠用**全 Unicode**（globstar 只折 ASCII） | `é` vs `É` ci=true           | match     | no-match       |
| 8   | `?` 按 **UTF-16 码元**（globstar 按字节）         | `?` vs `é`                   | match     | no-match(字节) |

**结论**：直接拿 picomatch 当 JS 侧，等于把上面 3.8% 的差异变成"dev 和 build 不一致"—— 这正是 globstar 要消灭的东西。**picomatch 这条路，作为"保证一致"的手段，被实测否决。**

(更进一步，如果想反过来"让 Rust 照搬 picomatch"——也不行：picomatch 生成的正则 17/22 用了 lookahead/lazy，**Rust 的 `regex` crate 设计上不支持**，只能上回溯引擎 `fancy-regex`，丢掉 2–7ns 的 DFA 优势，还在 build 路径上引入 **ReDoS**（pattern 来自项目配置 = 可被攻击）。详见方向 C1。)

---

## 4. 五条路线对比

| 方向                                                                                     |  终评分   | 关键利 / 关键弊                                                                                                                                                                                                                                                                                                                                         |
| ---------------------------------------------------------------------------------------- | :-------: | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **A — WASM/native 单一源**（同一份 Rust crate 编进 wasm 给 Node，rolldown 直接链接原生） |   **7**   | ✅ 唯一让漂移**结构上不可能**的方案；实测 wasm 仅 64KB（crate 零运行时依赖）、Node 同步加载 0.35ms、手写 FFI 复用缓冲 40ns/次（**比现在 JS 的 62ns 还快**）。❌ 收益**有条件**——两个消费方都还没用 globstar；要加 wasm 工具链；stock wasm-bindgen 的字符串 ABI 是 223ns/次的坑，必须手写 ~100 行胶水。                                                  |
| **D — 加固现状**（保留双引擎 + Rust stdin 差分入口 + JS fuzzer + 补薄弱语料）            |  **6.5**  | ✅ 最便宜、最低风险、**唯一真正能"量出"漂移**的事（~40–90 行，零新工具链）；是**所有其他方向的前置去风险步骤**（不先有它，你不敢删 JS 引擎，也不敢信序列化表）。❌ 不消除双重维护；提高的是"信心"而非"不可能"。                                                                                                                                         |
| **B — 代码生成/序列化 DFA 表**（Rust 编译出表，JS 用 ~150 行解释器跑）                   |   **5**   | ✅ 表是纯数据、方言规则全烤进去，picomatch 那 8 类差异不会复发；表很小（0.4–1.1KB）。❌ 对 Vite **同进程**拓扑是冗余的（既然 Node 里已有 Rust 编译器，直接调它就行，序列化是多余的第三件制品）；覆盖不到 `import.meta.glob` 真正用到的取反并集 / walker 剪枝 / facts；还多一份要锁步升级的格式契约。                                                    |
| **C2 — 共享 glob→正则翻译器**（两侧从一个小翻译器出正则，各用原生正则引擎）              | **2.5–6** | ✅ 翻译器仅 ~110 行，作为**测试用第三方 oracle 很值**（JS RegExp 跑到 3322/3322 + 100 万例全过）；JS 侧 is_match 又快又比 picomatch 更符合 spec。❌ 作为**运行时统一方案失败**：`dot=false`（walker 默认）要 lookaround，Rust regex **拒绝**；`match_dir` 没有布尔正则对应；Rust regex 比自研引擎慢 2–4 倍（reject 场景 22 倍）；JS RegExp 引入 ReDoS。 |
| **C2'(最小版) — JS 直接=picomatch**                                                      |  **2.5**  | ✅ 零成本、原生速度、Vite 已在用。❌ **正中下怀地制造**那 3.8% 不一致——就是要消灭的东西。                                                                                                                                                                                                                                                               |
| **C1 — 把 picomatch 定为权威方言，Rust 照搬**                                            |   **2**   | ✅ 一份生态通用方言。❌ 两个已验证的硬伤：Rust regex 编不了 picomatch 的 lookahead（被迫回溯 + build 侧 ReDoS）；`?`/字符类的 UTF-16 码元语义在 UTF-8 字节引擎上无法复刻。                                                                                                                                                                              |

---

## 5. 推荐方案：分阶段的"原生单一源 + 可执行一致性合约"

### 第 0 步（现在就做，方向 D —— 不管最终走哪条路都值）

写一个 **Rust stdin 差分入口**（读 `pattern\tpath\tflags`，用现有公开 API `Glob::new_with(...).is_match(path.as_bytes())` 输出 match/no-match/err:Kind）+ 一个 **Node 随机差分 fuzzer**，对随机 pattern+path 断言 JS==Rust。

- 这是**第一次**真正测量 JS↔Rust 漂移（agent 原型已经跑了 45 万例，0 漂移，证明 D 可行；且发现了"pattern 必须按 UTF-8 编码、path 按原始字节"这个坑）。
- 接进 CI（每 PR 固定种子 + 夜间长跑随机种子）。
- 顺便补薄弱语料：Windows 反斜杠、`dot=false` walker 模式、非 ASCII、≥6 分支并集。
- 把 fuzzer 抓到的每个差异**自动追加成语料行**（今天的随机发现 = 明天的回归锁）。
- 务必覆盖 `match_dir` 和多模式取反路径 —— 这俩是最容易漂、语料覆盖最薄的手写 JS 区域。

### 第 1 步（终态，方向 A 的改良版）

用 **native N-API（优先于 wasm）** 把**同一份 Rust crate** 同时给 Node 和 rolldown：

- 为什么 napi 优于 wasm：避免 wasm 每次 ~30ns 的"UTF-8 编码进线性内存"税（agent 实测这是 wasm 的主要开销），且能复用 **rolldown 已有的 napi 发布通道**（Vite 本来就装 rolldown）。没有预编译二进制的平台再退回现有 JS 引擎或 wasm。
- 删掉那 3723 行手写 JS 引擎，spec 改动只改一处（Rust），重编译就同时更新两端。
- **门槛**：fuzzer 全绿 + 全 3400 行语料对 native build 通过 + 至少一个消费方承诺接入。

### 同时（合约化，来自 completeness 评审的好点子）

- 把 `crates/globstar/tests/corpus` 升级成**带版本、跨语言的"一致性合约（conformance suite）"**（README 已经称它是"spec 的可执行形式"）。让 fast-glob、picomatch、rolldown 都能拿它来测。
- 加一个 **"Rust 当 oracle 生成语料"** 的工具：语料期望值现在是手写的，意味着**一个 spec bug 可能同时藏在 JS 和 Rust 里、还都通过差分**。用权威 Rust 引擎生成/扩充语料能堵这个洞。

### 明确否决

- **B**：对 Vite 同进程拓扑冗余，且漏掉真正用到的手写区。
- **C1**：Rust regex 编不了 picomatch + build 侧 ReDoS + UTF-16 语义无法复刻。
- **C2/picomatch 直用**：等于制造 3.8% 的 dev/build 不一致 —— 反目标。
- （C2 的翻译器**可以保留**，但只当**测试 oracle**，不进生产。）

---

## 6. 需要你拍板的关键问题

1. **目标到底是哪个？** (a) 只要 globstar 的 JS==Rust 内部一致；还是 (b) `import.meta.glob` 端到端 **dev==build** 一致？如果是 (b)，你是否愿意/能去推动 rolldown（fast-glob→globstar）和 Vite（picomatch+tinyglobby→globstar）的接入 PR？**没有接入，A 的单一源收益就用在一个没人用的库上，那 D 才是正确的终态。**
2. **spec §16 那些和 picomatch 的有意差异**（`a/**` 不匹配 `a`、`[!..]` 取反、空路径、字节级 `?`、ASCII-only 折叠）—— 是要坚持、即使接入时会改变某些用户的 glob 结果？还是其实你更想在安全的地方向 picomatch 靠拢（这会改变整个建议）？
3. **加 wasm32 + wasm-bindgen 工具链 + 一个提交进仓库的 .wasm** 能接受吗？还是"纯 cargo + 纯 node"是硬约束？（这是 A 的成本门槛；native napi 可以绕开 wasm 但需要每平台预编译二进制。）
4. **两个痛点哪个更痛**：潜在漂移 vs 双重维护？D 便宜地解决"漂移信心"但不解决维护；A 两个都解决但更贵。

---

## 附：可立刻动手的第一件事

不管 1–4 怎么答，**第 0 步（Rust stdin 差分入口 + JS fuzzer）在每条路线下都有价值、零工具链成本、~40–90 行**。我可以现在就把它写出来并接进 CI。
