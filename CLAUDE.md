# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repository is

**AII** 公链的**开发启动前文档资料包**。这里**只有 14 份 Markdown 文档**——还没有任何代码。AII 的 Rust 实现将放在另一个仓库（`github.com/AII-Network`，暂未创建）。

GitHub 远端：`github.com/kinglovesdao/aii` · 默认分支 `main`。

## 项目的不变设计基线（与每份文档一致）

修改任何文档前，先确认这些事实——它们贯穿全部文档，破坏其一会导致整套文档自相矛盾。

| 维度 | 设定 |
|---|---|
| **项目性质** | 完全去中心化的开源协议；**无公司、无基金会、无法律实体、无团队预挖、无投资人、无私募公募、无 DAO、无链上投票** |
| **主链共识** | 纯 PoS BFT（VRF 提议者 + ⅔ stake PRE-COMMIT 单区块即时最终性）。**无 PoW、无矿工、无 Ethash** |
| **代币模型** | 预约启动：60 天公开登记 → 10,000 名额 × 1,000,000 AII (100万) = 100亿 AII = 4.76% 创世；SECRC 多签 5 亿 AII (500M) = 0.24%；剩余 95% 通过 PoS 出块释放 |
| **总量上限** | 210,000,000,000 AII（原生大供应模型，BSC/TRON 级别的大供应量，**协议常量、不可变更**） |
| **开发语言** | Rust 全栈（含 revm / blst / arkworks / RocksDB / wasmtime） |
| **V 节点抵押 S_min** | **100,000 AII (10万)**（与预约启动 100 万 AII / 地址配套）|
| **区块奖励分配** | 80% 提议者 / 20% PRE-COMMIT 见证者（**无 DAO 国库份额**、**无保险基金份额**） |
| **协议升级** | Bitcoin / Linux 式 rough consensus —— 节点运营者自愿升级；**没有链上投票通道** |
| **SECRC** | 7 名 genesis 多签持有者，仅负责紧急安全热补丁 + 5 亿 AII (500M) 小额运营（**无任何治理决策权**） |
| **节点分级** | 7 Tier（T1 V 节点 → T7 移动钱包），客户端首启自动识别硬件 |
| **多端覆盖** | PC（Tauri 2）+ 移动（iOS Swift / Android Kotlin via UniFFI/JNI）+ 浏览器扩展（WASM）+ CLI |
| **AI 集成** | 协议级 CLI `aii` + MCP Server（差异化卖点）|

如果用户提出会破坏上述任一项的改动，先确认意图——这些是反复决策后的稳定基线。

## 文档组织（5 组 14 件）

```
00 术语表                              <- 共享术语基线，所有文档术语以此为准
A 项目基础（公开介绍）
  01 技术白皮书                        <- 对外主文档
  02 项目章程与创世声明                <- 项目宪法
B 协议规范（工程师阅读）
  03 黄皮书 — 形式化技术规范           <- 协议级精确定义
  04 架构设计文档                      <- 系统模块、接口、部署、多端客户端
  05 共识机制详细设计                  <- BFT-PoS + 节点 7 Tier 分级（详细）
C 经济与社区
  06 代币经济与分配模型                <- 预约启动 + 发行曲线
  07 社区与协议演进                    <- 无治理模式 + SECRC + 升级路径
D 工程与风险
  08 技术选型决策书                    <- Rust 栈选型理由
  09 安全与威胁模型                    <- 攻击向量 + Bug Bounty + SECRC
  10 开发路线图与里程碑                <- T0-12月 至 T0+36月（志愿者协作，非时间承诺）
E 开发者体验
  11 开发者文档与 SDK 规划             <- SDK 矩阵 + 示例 DApp
  12 AI 集成 — MCP 与 CLI              <- AI 原生公链差异化
```

文档间相互引用密集——修改一份时，先 `grep` 看其他文档是否引用了被改动的章节号/数据。

## 文档书写约定

- **跨文档引用**统一格式：`《NN 文档名》§X.Y`，如 `《04 架构设计文档》§7.1`。改文档编号时必须全仓库同步替换。
- **占位字段**：未最终决定的具体数字/字符串用 `《[占位] xxx》`，例如 `《[占位] T0》`、`《[占位] 0x...》`、`《[占位] 99》`（链 ID）。这些不是 bug——是有意保留待社区或 genesis 决定。
- **差异化决策**用 blockquote 块标注：`> **差异化决策**：xxx —— 理由：xxx`。便于评审。
- **版本号**：每份文档头部 `> 版本：v0.X`。重大重写时 bump（如 v0.3 → v0.4）。
- **取消的内容**保留为"v0.X 与 v0.Y 的差异说明"小节，便于回溯历史决策（特别是 07 治理与 12 章程末尾）。
- **不写**：CEO、CTO、创始人、顾问、投资人、基金会、公司、DAO、链上投票、提案系统、Treasury、Vesting、锁仓、Grants 委员会等术语——这些都不存在于 AII 协议。

## 修改文档的工作流

1. 改单份文档：用 `Edit` 工具，关键术语与跨引用先 `grep` 影响面。
2. 改影响多份文档的设计基线（共识、代币模型、治理模式等）：列出所有受影响段落，分文件 Edit；最后用 `grep` 验证残留。
3. 重命名/重排文档：用 `git mv`（保留历史），然后用 Python 脚本批量替换跨引用（参考 commit `bc77ec1` 的做法——占位符方案避免连锁替换）。
4. 写完后 commit + push 到 `origin/main`。

## Git 与 GitHub 推送

**关键：GitHub PAT 永远不进库**。`.gitignore` 已设规则禁止 `*token* *secret* *credential* .env* *.key`。

推送的标准方法（PAT 一次性传入 credential helper，不写入 `.git/config`）：

```bash
GH_TOKEN='<PAT>' git -c credential.helper='!f() { echo "username=x-access-token"; echo "password=$GH_TOKEN"; }; f' push origin main
```

PAT 用户保管在密码管理器，**不要**保存到本地文件。

网络层注意：用户网络环境的 github.com 解析为 `198.18.0.43`（fake-IP 代理）。推送失败时常见原因是代理状态波动，等几秒重试通常恢复。

## 文档发布到 docx / PDF

```bash
# 单份转 docx
pandoc 01_技术白皮书.md -o 01_技术白皮书.docx

# 单份转 PDF（中文需 xelatex + 中文字体）
pandoc 01_技术白皮书.md -o 01_技术白皮书.pdf --pdf-engine=xelatex \
  -V CJKmainfont="Noto Sans CJK SC"

# 全集合并为单 PDF（带目录）
pandoc 0?_*.md 1?_*.md -o AII_完整文档.pdf --pdf-engine=xelatex \
  -V CJKmainfont="Noto Sans CJK SC" --toc
```

`Cargo.toml` / `package.json` 等不存在——本仓库无构建系统、无 lint、无测试，文档纯 Markdown。

## 与 MOAC 原始资料的关系

`../MOAC/`（仓库外）保留原始 MOAC 白皮书与衍生项目（GOD 公链）的 PPT/DOCX/PDF 作为参考素材。**只读，不修改**。AII 以 MOAC 分层多链架构为蓝本，但在共识、治理、代币、语言、AI 集成等所有维度**全面差异化**。

## 用户偏好（已确认）

- 文档语言：**纯中文**（关键术语保留英文原文）
- 文档深度：完整初稿（每份数千到上万字，非大纲）
- 推送 GitHub 后**立刻提示用户撤销并轮换 PAT**——任何在对话中明文出现的 token 都视为已泄露
