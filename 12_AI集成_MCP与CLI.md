# 12 AII AI 集成层 — MCP Server 与 CLI

> 版本：v0.1（初稿）
> 适用对象：AI Agent 开发者、自动化运维工程师、DApp 工程师、终端用户
> 关联：本文档为 AII 项目的**差异化能力**——AI 原生公链。与《01 技术白皮书》§7 API、《04 架构设计文档》§14 客户端架构、《11 开发者文档与 SDK 规划》配套阅读。

---

## 1. 设计目标

> **AII 是首条把"AI Agent 作为一等公民"的公链。**

AII 协议层提供两条平行接入轨：

- **CLI（Command-Line Interface）**：所有链上操作通过 `aii` 命令完成，让任何能调用 Shell 的 AI 智能体（Claude Code、Cursor agent、自定义 LLM Bash tool 等）**零样板**操作链。
- **MCP Server（Model Context Protocol）**：AII 节点内建 MCP 协议服务，让 Claude Desktop、Claude Code、OpenDevin、Cline、任何 MCP 兼容客户端**即插即用**地把 AII 当作工具集调用。

| 接入轨 | 谁来用 | 调用范式 |
|---|---|---|
| **CLI（`aii`）** | 人类工程师、Shell 脚本、Bash-tool AI 智能体 | 命令行参数 |
| **MCP Server** | LLM Agent、Copilot、Cursor、Claude Code/Desktop | 结构化工具调用（JSON-RPC） |
| RPC（JSON-RPC） | DApp、SDK、传统集成 | 同以太坊 |

三轨**共享底层 `aii-core` Rust 库**，无功能差异，只是接入面不同。

---

## 2. 为什么 AI 原生重要

### 2.1 行业趋势

- 2024 年起 AI Agent 全面渗透工程实践（Claude Code、Cursor、Devin、OpenHands 等）
- LLM 已能写 Solidity、Rust、读懂区块链文档
- 制约 Agent 上链的瓶颈不是"模型能力"，而是**工具接入摩擦**：
  - 复杂 SDK 学习成本
  - 私钥管理风险
  - 网络/RPC 配置复杂度
  - 错误信息对 LLM 不友好

### 2.2 AII 的差异化

| 维度 | 主流公链 | AII |
|---|---|---|
| AI 接入方式 | SDK / RPC，Agent 须先学习 ABI | **CLI 或 MCP，零学习成本** |
| 错误信息 | hex 错误码、stack trace | 结构化、可读、含 fix 建议 |
| 安全模型 | 私钥直接传 SDK | **本地隔离签名 + 用户确认** |
| 离线签名 | 需要专门工具 | CLI 一行 + MCP 标准 |
| Agent 文档 | 给开发者看的 | **专门为 LLM 写的 prompt-tuned 文档** |

> **设计哲学**：**链的可用性 = max(人类可用性, AI 可用性)**。AII 同等优化人类与 AI 的访问体验。

---

## 3. CLI 设计：`aii` 命令

### 3.1 命令族（v1.0 GA 范围）

```
aii <subcommand> [options]
```

| 子命令 | 用途 |
|---|---|
| `aii wallet` | 钱包：生成、导入、列出、签名、解锁 |
| `aii account` | 账户：查询余额、nonce、状态 |
| `aii tx` | 交易：构造、签名、广播、查询、回执 |
| `aii contract` | 合约：编译、部署、调用、ABI 编解码 |
| `aii block` | 区块：查询、订阅、统计 |
| `aii state` | 状态：查 storage slot、code、proof |
| `aii subchain` | 子链：列表、创建、加入、查 Flush 状态 |
| `aii crosschain` | 跨链：发送、追踪、HTLC 构造 |
| `aii node` | 节点：状态、peers、同步进度、Tier 切换 |
| `aii dao` | 治理：列提案、投票、提案、国库 |
| `aii dev` | 开发：编译、deploy、test、模拟交易 |
| `aii faucet` | 测试网水龙头领币 |
| `aii ai` | 启动/管理 MCP server |
| `aii config` | 客户端配置：网络、RPC 端点、默认账户 |
| `aii completion` | 生成 bash/zsh/fish/pwsh 补全脚本 |

### 3.2 设计原则

- **可组合**：所有输出默认机器可读（JSON），可与 `jq` / `awk` 拼接
- **可解释**：每条命令支持 `--explain` 输出"这条命令在做什么"（给 LLM 阅读）
- **可预演**：写操作支持 `--dry-run`（构造交易但不广播）+ `--simulate`（本地模拟执行）
- **错误友好**：错误输出含 `code`、`message`、`hint`、`docs_url` 四元
- **零配置**：自动发现本地节点；如无则连默认社区 RPC
- **安全默认**：私钥永远不出现在 argv（避免被记入 shell history / ps）；通过 `--keyfile` / 交互输入 / 环境变量传入

### 3.3 输出格式标准

CLI 输出可选 `--output` 参数：

| 格式 | 用途 |
|---|---|
| `human`（默认） | 人类可读表格/段落 |
| `json` | 机器可读，单行 JSON |
| `json-pretty` | 多行美化 JSON |
| `yaml` | YAML 格式 |
| `csv` | CSV（仅列表类输出） |
| `markdown` | Markdown 表格/段落（给 LLM 友好） |

**LLM 友好默认**：当检测到环境变量 `AII_OUTPUT=llm` 或 `--llm` 时，输出 markdown 格式 + 包含上下文摘要。

### 3.4 命令示例

```bash
# 创建新钱包
$ aii wallet new --name alice
✓ Created wallet 'alice' at ~/.aii/keystore/alice.json
  Address: 0x7c8f...3a92
  Mnemonic shown in this terminal only — save securely.

# 查询余额
$ aii account balance 0x7c8f...3a92
0x7c8f...3a92 → 1234.56 AII (主链) | DAO 投票权: 1234.56 + 抵押 0

# 部署合约
$ aii contract deploy ./MyToken.sol --from alice --gas-limit 2000000 --dry-run
✓ Compiled MyToken.sol (Solidity 0.8.24)
✓ Estimated gas: 1,234,567
✓ Would deploy to: 0xaaa...bbb（CREATE 预测地址）
  Dry run only. Add --confirm to actually send.

# 列出活跃子链
$ aii subchain list --status active --output markdown
| ID                | 名称        | 共识 | TPS   | SCS 节点数 |
|-------------------|-------------|------|-------|----------|
| 0xabc...          | DeFi-Hub    | PBFT | 3,200 | 21       |
| 0xdef...          | GameWorld   | DPoS | 8,500 | 21       |

# 启动 MCP server（默认 stdio，仅给本机 AI 使用）
$ aii ai serve
✓ AII MCP server listening on stdio
  Available tools: 24 read-only + 12 write
  Connect with Claude Desktop / Claude Code / Cursor.
```

### 3.5 自动补全

支持五大 shell 的命令/参数/枚举值补全：

```bash
$ aii completion bash > /etc/bash_completion.d/aii
$ aii completion zsh > ~/.zsh/completions/_aii
$ aii completion fish > ~/.config/fish/completions/aii.fish
$ aii completion powershell > $PROFILE.CurrentUserCurrentHost
```

---

## 4. MCP Server 设计

### 4.1 MCP 是什么

[Model Context Protocol](https://modelcontextprotocol.io) 是 Anthropic 2024 年发布的开放协议，定义了 LLM 智能体与外部工具/资源的标准化对话格式。Claude Desktop、Claude Code、Cursor、Cline、OpenDevin 等主流 AI 客户端原生支持。

AII 内建 MCP server，**任何 MCP 客户端可零代码接入**：

```
┌──────────────┐    JSON-RPC      ┌─────────────────┐    aii-core    ┌──────────┐
│ Claude       │ ───── over ────► │  aii ai serve   │  ───────────►  │  AII     │
│ Desktop /    │   stdio/HTTP+SSE │  (MCP Server)   │                │  Node    │
│ Claude Code  │                  │                  │                │          │
└──────────────┘                  └─────────────────┘                └──────────┘
```

### 4.2 工具集（Tools）

MCP 工具分为**只读**（自动批准）与**写入**（需用户每次确认）两类：

#### 只读工具（read-only，22 个）

| 工具名 | 功能 |
|---|---|
| `aii_get_balance` | 查询某地址 AII 主链余额 |
| `aii_get_block` | 按号/哈希查区块 |
| `aii_get_transaction` | 按哈希查交易 + 回执 |
| `aii_get_receipt` | 查交易回执 + logs |
| `aii_get_code` | 查地址处合约字节码 |
| `aii_get_storage` | 查指定 storage slot |
| `aii_get_nonce` | 查地址当前 nonce |
| `aii_estimate_gas` | 估算交易 gas |
| `aii_call_contract` | 只读合约调用（eth_call） |
| `aii_decode_calldata` | 用 ABI 解码 calldata 为可读 |
| `aii_decode_event` | 用 ABI 解码事件 log |
| `aii_list_microchains` | 列出活跃子链 |
| `aii_get_microchain_state` | 查某子链当前状态根 |
| `aii_get_flush_history` | 查某子链 Flush 历史 |
| `aii_get_vnode_set` | 查当前 V 节点集合 |
| `aii_get_dao_proposals` | 列 DAO 提案 |
| `aii_get_dao_treasury` | 查国库余额与近期支出 |
| `aii_get_node_status` | 查本地节点同步、peer、Tier 状态 |
| `aii_get_chain_info` | 查链 ID、出块时间、难度 |
| `aii_get_gas_price` | 查当前推荐 gas price（含 EIP-1559 baseFee） |
| `aii_search_address` | 文本搜索地址（含 ENS-like 服务） |
| `aii_help` | 列出所有工具并返回简介 |

#### 写入工具（write，需确认，10 个）

| 工具名 | 功能 |
|---|---|
| `aii_send_transaction` | 构造、签名并广播交易 |
| `aii_deploy_contract` | 部署合约 |
| `aii_call_contract_tx` | 调用合约（写入） |
| `aii_stake_vnode` | 抵押成为 V 节点 |
| `aii_unstake_vnode` | 退出 V 节点 |
| `aii_vote_proposal` | DAO 投票 |
| `aii_create_proposal` | 创建 DAO 提案 |
| `aii_create_microchain` | 创建子链 |
| `aii_join_microchain` | 加入子链作为 SCS |
| `aii_cross_chain_send` | 发送跨链消息 |

#### 资源（Resources，按 MCP 标准暴露）

- `aii://docs/whitepaper` → 完整白皮书
- `aii://docs/api-reference` → API 参考
- `aii://chain/info` → 当前链状态摘要
- `aii://accounts/{address}` → 账户详细信息
- `aii://contracts/{address}` → 合约源码（如已验证）+ ABI
- `aii://blocks/{number_or_hash}` → 区块详情

### 4.3 安全模型

**核心原则：私钥永远在本地，AI 永远不接触**

```
┌─────────────────────────────────────────────────────────────┐
│ AI Agent 调用 aii_send_transaction:                         │
│ {                                                          │
│   "to": "0xabc...",                                        │
│   "value": "1.0 AII",                                      │
│   "data": "0x...",                                         │
│   "from": "alice"  ← 钱包别名，不传私钥                       │
│ }                                                          │
└──────────────────┬──────────────────────────────────────────┘
                   │
                   ▼
┌─────────────────────────────────────────────────────────────┐
│ MCP Server 本地处理：                                         │
│ 1. 构造交易、估算 gas、模拟执行                                  │
│ 2. 向用户弹出确认对话框（桌面通知 / 终端 prompt / 移动推送）：       │
│                                                              │
│    ⚠️ AI 请求发送交易：                                        │
│       From   : 0x7c8f...3a92 (alice)                        │
│       To     : 0xabc...                                     │
│       Value  : 1.0 AII (~$5.23)                             │
│       Gas    : 0.0021 AII (~$0.01)                          │
│       Calling: transfer(0xdef..., 1000000)                  │
│                                                              │
│    [Approve] [Reject] [Approve & Remember 5 min]            │
│                                                              │
│ 3. 用户批准后，钱包模块解锁私钥并签名（私钥从未离开本地内核）      │
│ 4. 广播交易，返回结果给 AI                                       │
└─────────────────────────────────────────────────────────────┘
```

### 4.4 传输层

| 模式 | 触发 | 用途 |
|---|---|---|
| **stdio**（默认） | `aii ai serve` | 同机 AI 客户端（Claude Desktop 标准） |
| **SSE over HTTP** | `aii ai serve --listen 127.0.0.1:5099` | 本机其他进程 |
| **HTTPS + token** | `aii ai serve --listen 0.0.0.0:5099 --tls --token <hex>` | 远程 Agent（需显式开启） |

默认绑定 `127.0.0.1`，**永不**默认对外暴露。

### 4.5 客户端集成示例

#### Claude Desktop（macOS / Windows）

`~/Library/Application Support/Claude/claude_desktop_config.json`：

```json
{
  "mcpServers": {
    "aii": {
      "command": "aii",
      "args": ["ai", "serve"]
    }
  }
}
```

重启 Claude Desktop 后，对话框内即可直接调用 AII 工具：

> 用户：帮我查一下 alice 钱包的余额，然后给 0xabc... 转 0.5 AII。
> Claude：[调用 aii_get_balance] → 余额 12.34 AII。[调用 aii_send_transaction]（弹确认）→ 已发送，tx hash 0x...

#### Claude Code（CLI）

```bash
claude mcp add aii -- aii ai serve
```

#### Cursor / Cline / OpenDevin

按各自 MCP 配置规范填入 `aii ai serve` 命令。

### 4.6 工具发现协议（特色扩展）

AII MCP server 额外提供 `aii_help` 工具，LLM 首次接入时调用即可获得**针对当前 AII 协议版本**的完整工具清单、参数 schema、用法示例。避免硬编码工具集导致的版本漂移。

---

## 5. 实现选型

| 组件 | 选型 | 理由 |
|---|---|---|
| CLI 框架 | **clap v4** + 派生宏 | Rust 生态标杆，支持自动补全、子命令、derive、color、错误提示 |
| MCP 服务 SDK | **rmcp**（Rust 官方 MCP SDK） | Anthropic / Modelcontextprotocol.io 维护 |
| JSON-RPC | jsonrpsee 或自研薄封装 | 与 `aii-core` 直接对接 |
| 配置 | `~/.aii/config.toml` + 环境变量 | 跨平台标准 |
| 日志 | tracing + tracing-subscriber | 可结构化输出，便于 AI 解析 |
| 凭据管理 | OS native keystore（Keychain / DPAPI / Secret Service） | 安全默认 |

二进制布局：

```
crates/
├── aii-core/         # 节点内核（与 §03 一致）
├── aii-cli/          # CLI 实现（clap）
├── aii-mcp/          # MCP server 实现（rmcp）
└── aii-wallet/       # 共享钱包/签名

apps/
├── aiid             # 守护进程（节点）
├── aii              # 用户面 CLI（含 `aii ai serve` 子命令启动 MCP）
└── desktop/         # Tauri 桌面应用（前端 UI + 嵌入 aii-cli + aii-mcp）
```

> **关键决策**：MCP server **不是**独立二进制——它是 `aii ai serve` 子命令，复用同一个 `aii` 用户面入口，避免用户对多个二进制的混淆。

---

## 6. 文档与开发者体验

### 6.1 给 LLM 看的"工具卡"

每个 MCP 工具的 description 字段按以下格式写（避免冗余、避免歧义）：

```yaml
name: aii_send_transaction
description: |
  Send AII tokens or call a contract from a local wallet.
  
  Use when the user explicitly asks to send/transfer/pay/call.
  Requires user confirmation via local approval prompt.
  Returns: { tx_hash, status, gas_used, ... }
  
  Common errors:
    INSUFFICIENT_FUNDS    → wallet balance too low
    NONCE_TOO_LOW         → tx already mined or wallet not synced
    GAS_LIMIT_TOO_LOW     → increase gas_limit or call estimate_gas first

parameters:
  from: string  (wallet alias or address; private key never sent)
  to: string    (target address, or null for contract creation)
  value: string (amount in AII, e.g. "1.0" or "0.5")
  data: string  (optional, hex-encoded calldata)
  gas_limit: number (optional)
  
examples:
  - { from: "alice", to: "0xabc...", value: "1.0" }
  - { from: "alice", to: "0xtoken...", value: "0", data: "0xa9059cbb..." }
```

### 6.2 LLM 提示词模板

社区维护 `aii-llm-prompts` 仓库，包含：

- 系统提示词（System Prompt）模板：让 LLM 理解 AII 体系
- 常见任务示例（Few-Shot Examples）：转账、查询、合约部署、DAO 投票
- 错误处理范式
- 安全提示模板

任何 AI 智能体集成 AII 时可一行引用。

---

## 7. 路线图

| 阶段 | 里程碑 | 时间 |
|---|---|---|
| MVP | `aii` CLI 覆盖 wallet/tx/account 核心命令；MCP server 5 个只读工具 | 主网启动 |
| GA | 全部 CLI 子命令 + 32 个 MCP 工具 | 主网 + 1 月 |
| v1.1 | Claude Desktop / Cursor 一键安装包；中文/英文 LLM 提示词官方版 | 主网 + 3 月 |
| v1.5 | LLM 评测套件：对 GPT-4 / Claude / Gemini / DeepSeek 测试 AII 操作正确率 | 主网 + 6 月 |
| v2.0 | Agent-to-Agent 直接结算协议（DAO-paid AI Agent 工作流） | 主网 + 12 月 |

---

## 8. 安全审计要点

主网启动前对 AI 集成层的专项审计：

1. **私钥隔离**：所有路径下私钥**仅**经 `aii-wallet` 模块；MCP 工具签名前必须经用户确认
2. **确认 UI 防伪**：交易确认对话框需展示**真实**目标地址（防 unicode 同形）、真实金额、真实 calldata 解码
3. **远程 token 防滥用**：HTTPS+token 模式默认 IP 白名单 + 速率限制
4. **MCP 工具输出净化**：返回给 LLM 的字符串过滤 prompt-injection 模式
5. **日志最小化**：默认不记录 calldata、不记录余额，避免本地日志泄露

---

## 9. 与传统公链的差异

| 维度 | 主流公链 | AII |
|---|---|---|
| 命令行工具 | 有（geth attach / solana / cast） | 有（`aii`）+ **专为 AI 友好设计** |
| AI 接入 | 通过 SDK / 第三方包装 | **协议级原生 MCP server** |
| LLM 错误信息 | hex 错误码 | 结构化 + hint + docs_url |
| 私钥安全 | 用户自己写代码隔离 | **协议级"确认即签"**架构 |
| Agent 工作流 | 需要自己拼装 | 资源/工具/Prompt 三件套官方维护 |

---

## 10. 决策待回填

| # | 项 | 候选 | 截止 |
|---|---|---|---|
| 1 | MCP server 默认端口（如启用 HTTP） | 5099 / 8788 | T0-3 月 |
| 2 | 是否默认随 `aiid` 自动启动 | 是 / 否 | T0-3 月 |
| 3 | 中文 LLM 提示词维护责任方 | 社区 / DAO Grant | T0-1 月 |
| 4 | MCP 工具版本管理（链上注册？） | – | Phase 5+ |

---

— 本 AI 集成（MCP 与 CLI）设计完 —
