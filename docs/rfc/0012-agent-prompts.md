# RFC-0012：复用 .agents 下的 Skills 与 Prompts

日期：2026-10-27。当前排期只以 [task_plan.md](../../task_plan.md) 为准；本文说明设计与验收约定。

## 目标与已有能力

希望在 ChatGPT 等通过 gld 连接的客户端读取本机通用的 `~/.agents/skills` 和 `~/.agents/prompts`，
不需要把同一套指导文件复制进各 AI 客户端。优先复用已有 Skill 发现、按需加载和限界访问能力。

**已经实现的部分：**`list_skills` / `get_skill` 默认会扫描 `~/.agents/skills` 与项目里的
`.agents/skills`，对链接、重复内容、不可读文件和 `disable-model-invocation` 有明确规则；
路径与权限见 [concepts.md](../concepts.md#本机和项目里的-skill)。不为相同文件重写发现器。

**本次缺口：**本机已存在 `~/.agents/prompts/*.md`，但 gld 没有可按需列举/读取 Prompt 的工具。
Prompt 是可复用的文本模板，不等于含 `SKILL.md` 的技能包，也不等于自动生效的 `AGENTS.md`。
文档的指令内容只作为用户选择的素材返回，**读取不会代替用户授予执行权限**。

## 先做什么

新增唯一入口 `prompt_manage`，只读，动作 `list`（名称、来源、描述）与 `read`（按
`id` 或明确的 `name` 取原始 Markdown），供默认 compact、完整和只读工具档使用。
不增加模型编排、自动执行、模板变量插值或第二套 Skill 格式。

| 路径 | 规则 |
| --- | --- |
| `~/.agents/prompts/*.md` | 用户级；仅完整授权且已认证的客户端或本机 CLI 可见 |
| `<workspace>/.agents/prompts/*.md` | 项目级；遵守已有工作区可见范围，匿名入口仍可访问自己已开放的项目 |
| `~/.agents/skills/*/SKILL.md` 与项目同类路径 | 已支持；保持 `list_skills` / `get_skill` 原语义 |

首版只扫描目录第一层的普通 `.md` 文本文件。隐藏文件、目录、其他扩展名、
超出单文件限制的内容不提供正文；项目内指向目录外的链接不能绕过读取范围；
用户级 symlink 也暂不读取，避免新接口间接跨到其他个人文件。
同名 Prompt 用 ID 或 scope 明确选择，不静默选错全局/项目版本；ID 与文件路径绑定，正文以
`contentSha256` 标记版本，避免内容变动后仍误用旧模板。

返回的 `description` 和 `argumentHint` 来自可选 YAML frontmatter；没有 frontmatter
也能列举和读取。原始文本可能包含 `$ARGUMENTS` 等变量，**首版不做替换**，交由用户或
调用客户端处理。其他家 Codex 的 `~/.codex/prompts` 属于旧格式兼容来源，暂不扩大默认扫描；
同样不实现自动挂载 MCP `prompts/list` 或斜杠菜单（不同客户端支持程度不一）。

## 安全与异常

1. 用户级 Prompt 含私有指令，不能因为授予某个项目 read-only grant 或开了 noauth，
   就让该连接读取整个用户主目录的 Prompt。权限先过滤候选，再处理 ID/名称，避免侧漏。
2. 仅可按已发现的 Prompt ID 或名称读取，不接收任意路径；文件扫描、读取均有边界；
   拒绝路径穿越和逃逸链接，不暴露隐藏文件内容。读取时重新检查实际文件，不能只相信旧列表。
3. Prompt 属于**不可信工作内容**：其中自称高优先级、要求披露隐私或执行命令的文字
   不会成为 gld 的系统权限；不在 MCP server 初始化说明中直接展开全部 Prompt。
4. 缺目录返回空集合；坏 UTF-8、无法读取、过大等明确列出失败原因；
   重名要求按 ID/scope，找不到明确返回错误。避免把本机绝对路径写入公开示例和操作日志。
5. 不修改用户原始文件、现有 Skill 目录、服务凭据和持久化配置；新增工具表只需客户端
   Refresh，不要求删除连接器、重新注册或更换 OAuth client_id。部署按现有流程另行授权。

## 开发切片与验收

| 阶段 | 内容 | 完成标准 |
| --- | --- | --- |
| D15-P0 | RFC、任务队列和既有 Skill 现状核对 | 明确两个目录的边界、权限、默认配置、测试与部署约束 |
| D15-P1 | `prompt_manage` 只读接口，工具注册和测试 | 用隔离 home/workspace 夹具验证 list/read、同名、更新生效、空目录、非法扩展、链接、超大文件、匿名与 grant 拒绝及所有工具档可见 |
| D15-P2 | CLI/MCP 与真实机器联调、文档收尾 | 不输出真实模板正文地核对本机目录条目；完整鉴权连接器 Refresh 后按名字读取指定模板；必要时再讨论官方 MCP Prompt 原语、变量扩展与历史兼容来源 |

一次实现一项，附测试后粒度提交。运行服务换装、真实连接器 Refresh 和用户级权限修改
不在编码授权内。通过 P1 不代表当前已经连接的旧守护进程提供了新工具；状态看任务队列。

## 来源

- [Codex Skills 目录约定](https://learn.chatgpt.com/docs/customization/overview)：
  用户级 `~/.agents/skills` 与项目级 `.agents/skills`。
- [Codex Custom Prompts（旧机制）](https://learn.chatgpt.com/docs/custom-prompts)：
  单个 Markdown 模板、可选描述和参数提示；现已建议迁移为 Skills。`~/.agents/prompts`
  是本项目明确要兼容的本地位置，不宣称 Codex 默认自动从这里读取。
- 已有实现：`crates/core/src/agent_context.rs`、`tools/skill.rs`、
  `tools/registry.rs` 和 [安全约定](../security.md)。
