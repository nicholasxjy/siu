# siu

一个终端 UI（基于 [ratatui](https://github.com/ratatui/ratatui)），统一管理本机各个 coding agent 的 **MCP servers** 和 **skills**：安装、更新、按 agent 开启/关闭、暂停、删除，并支持在线搜索 MCP Registry 与 skills.sh。

设计参考 [magpie](https://github.com/yetone/magpie) 的 Library：一处维护，按每个 agent 自己的格式写入它自己的配置文件；**只删除 siu 自己写入的内容**，文件里的其它内容（注释、顺序、其它 server）保持不变。

## 支持的 agent

只显示本机已安装的（检测其配置目录）。

| Agent | MCP 配置 | Skills 目录 |
| --- | --- | --- |
| Claude Code | `~/.claude.json`（遵循 `CLAUDE_CONFIG_DIR`） | `~/.claude/skills` |
| Codex | `~/.codex/config.toml`（遵循 `CODEX_HOME`，保留注释） | `~/.codex/skills` |
| Gemini CLI | `~/.gemini/settings.json` | `~/.gemini/skills` |
| OpenCode | `~/.config/opencode/opencode.json` | `~/.config/opencode/skills` |
| Cursor | `~/.cursor/mcp.json` | `~/.cursor/skills` |
| Copilot CLI | `~/.copilot/mcp-config.json` | `~/.copilot/skills` |
| Droid | `~/.factory/mcp.json` | `~/.factory/skills` |
| Claude Desktop | `claude_desktop_config.json`（仅 stdio） | — |
| 共享目录 | — | `~/.agents/skills` |

## 安装与运行

```sh
cargo install --path .
siu
```

数据保存在 `~/.siu`（可用 `SIU_HOME` 修改）：

- `library.json`：所有 server / skill、各自开启的 agent、以及 siu 在每个 agent 中写入过什么
- `skills/<name>/`：skill 文件；各 agent 的 skills 目录中放的是指向这里的软链接，更新一次所有 agent 同时生效
- `backups/`：siu 第一次修改某个配置文件前的原始副本；导入 skill 时 agent 中原有的目录也移到这里
- `cache/`：skills.sh 热门榜（6 小时后后台刷新）与 skill 描述，下次打开即刻显示

## 主题

自动跟随终端的深色/浅色背景（启动时询问终端背景色，取不到时参考 `COLORFGBG`，默认深色）。配色基于 Catppuccin Mocha / Latte，并按 WCAG 调整：所有文字颜色在常见深色与浅色背景上的对比度都 ≥ 4.5:1（由测试保证）；不支持真彩色的终端自动换用最接近的 256 色。

- `t`：在深色与浅色之间切换
- `SIU_THEME=dark` 或 `SIU_THEME=light`：固定使用其中一种

## 使用

三个标签页：`1` MCP · `2` Skills · `3` Discover（`tab` 切换，`?` 查看全部按键）。

**MCP / Skills**

- `⏎` 进入右侧 agent 面板，`space` 逐个开关，`a` 全开，`n` 全关
- `space`（列表中）暂停/恢复：从所有 agent 中移除，但保留选择
- `a` 新增：MCP 为表单（stdio / http / sse）；Skills 可填 `owner/repo`、GitHub 链接（如 `…/tree/main/skills/pdf`）或本地目录
- `e` 编辑 server，`d` 删除，`u` / `U` 更新当前 / 全部 skill，`/` 过滤
- 列表下方 “found in agents” 是 agent 中已有、但不由 siu 管理的条目；`⏎` 导入后即可统一管理（siu 在导入前不会改动它们）

**Discover**

- MCP：内置精选列表即时过滤，输入停顿后自动搜索官方 [MCP Registry](https://registry.modelcontextprotocol.io)
- Skills：默认展示 [skills.sh](https://skills.sh) 热门榜（启动时后台预取并缓存）；输入时先即刻显示本地索引（约 600 个热门 skill）中的匹配，skills.sh 的结果随后合并进来，同一会话内重复搜索直接命中缓存；光标附近的描述并行预取；光标停留约 1 秒会预下载该仓库，同一仓库 10 分钟内再次安装无需重新下载
- `←/→` 切换 MCP / Skills，`⏎` 安装（需要 API key 等参数时会弹出表单），默认开启给所有可用的 agent

## 开发

```sh
cargo test                       # 单元测试 + 以按键驱动的 E2E 测试（离线、临时 HOME）
cargo test -- --ignored online   # 联网 E2E：真实搜索 Registry/skills.sh 并从 GitHub 安装
SIU_SNAPSHOT=out.html cargo test -- --ignored snapshot   # 把各界面在深/浅主题下渲染成彩色 HTML，便于检查配色
```
