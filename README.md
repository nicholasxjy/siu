# siu

在终端里统一管理本机各 coding agent 的 **MCP servers** 和 **skills**：安装、更新、按 agent 开关、暂停、删除，并可在线搜索 [MCP Registry](https://registry.modelcontextprotocol.io) 与 [skills.sh](https://skills.sh)。

设计参考 [magpie](https://github.com/yetone/magpie) 的 Library：一处维护，按各 agent 自己的格式写入它的配置。siu **只改动自己写入的内容**，配置文件里的注释、顺序和其它条目保持不变。

## 安装

```sh
cargo install --path .
siu
```

## 支持的 agent

只显示本机已安装的。

| Agent | MCP 配置 | Skills 目录 |
| --- | --- | --- |
| Claude Code | `~/.claude.json`（遵循 `CLAUDE_CONFIG_DIR`） | `~/.claude/skills` |
| Codex | `~/.codex/config.toml`（遵循 `CODEX_HOME`） | `~/.codex/skills` |
| Gemini CLI | `~/.gemini/settings.json` | `~/.gemini/skills` |
| OpenCode | `~/.config/opencode/opencode.json` | `~/.config/opencode/skills` |
| Cursor | `~/.cursor/mcp.json` | `~/.cursor/skills` |
| Copilot CLI | `~/.copilot/mcp-config.json` | `~/.copilot/skills` |
| Droid | `~/.factory/mcp.json` | `~/.factory/skills` |
| Claude Desktop | `claude_desktop_config.json`（仅 stdio） | — |
| 共享目录 | — | `~/.agents/skills` |

## 使用

三个标签页：`1` MCP · `2` Skills · `3` Discover。`?` 查看全部按键。

| 按键 | 作用 |
| --- | --- |
| `⏎` | 选择 agent（面板内 `space` 开关，`a` 全开，`n` 全关） |
| `space` | 暂停 / 恢复：从所有 agent 移除，保留选择 |
| `a` | 新增：server 填表单；skill 填 `owner/repo`、GitHub 链接或本地目录 |
| `e` / `d` / `D` | 编辑 server / 删除 / 清空当前标签页（需确认） |
| `u` / `U` | 更新当前 / 全部 skill |
| `/` | 过滤 |
| `t` | 切换深色 / 浅色主题 |

- **分组**：skills 按来源分组（GitHub 仓库、本地目录、从 agent 导入）。超过 10 个的组默认折叠，在组标题上按 `⏎` 或 `←` / `→` 折叠和展开。
- **导入**：列表下方 “found in agents” 是 agent 里已有、但不由 siu 管理的条目，按 `⏎` 导入。导入前 siu 不会改动它们。
- **Discover**：`/` 搜索，`←` / `→` 切换 MCP 与 skills，`⏎` 安装到所有可用的 agent。

## 配置

| 环境变量 | 作用 |
| --- | --- |
| `SIU_HOME` | 数据目录，默认 `~/.siu` |
| `SIU_THEME` | `dark` 或 `light`；默认跟随终端背景 |
| `ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY` | 网络代理，支持 `socks5://`，遵循 `NO_PROXY` |

数据目录内容：

- `library.json`：所有条目、各自开启的 agent，以及 siu 在每个 agent 中写入了什么
- `skills/`：skill 文件；各 agent 中是指向这里的软链接，更新一次全部生效
- `backups/`：首次修改配置前的原文件，以及导入 skill 时替换下来的目录
- `cache/`：skills.sh 热门榜、skill 描述、分组折叠状态

## 开发

```sh
cargo test                                               # 单元测试与按键驱动的 E2E（离线）
cargo test -- --ignored online                           # 联网 E2E
SIU_SNAPSHOT=out.html cargo test -- --ignored snapshot   # 渲染各界面为 HTML，检查配色
```
