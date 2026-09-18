# ccs-proxy

`ccs-proxy` 是一个本地转发工具。

本地入口默认使用 HTTPS/WSS，将请求转发到配置的 HTTP/HTTPS 上游服务。TLS 直接由 `ccs-proxy` 提供，不需要 Caddy 等额外服务。新版 Codex App 的工作区路由要求 HTTPS，请使用下方 TLS 配置。

## 什么时候需要它

你需要让 Codex App 访问一个自定义的 ChatGPT/Codex 上游服务时，可以用它。

默认使用：

```text
https://localhost:8000
```

不要随便改成 `127.0.0.1` 或其他端口。部分 Codex App 版本只会给 `localhost:8000` 自动带上 ChatGPT 登录头，换掉后可能出现上游返回 `401`、`403` 或连接失败。

## 使用步骤

### 步骤一：下载或自己编译

推荐直接下载发布包。只有在你想自己改代码，或发布页没有适合你系统的包时，才需要自己编译。

#### 方式 A：下载发布包

每次发布版本都会在 GitHub Releases 里提供 6 个二进制包：

[https://github.com/claudecode-store/ccs-proxy/releases](https://github.com/claudecode-store/ccs-proxy/releases)

| 系统    | 架构  | 文件                           |
| ------- | ----- | ------------------------------ |
| Linux   | amd64 | `ccs-proxy-linux-amd64.tar.gz` |
| Linux   | arm64 | `ccs-proxy-linux-arm64.tar.gz` |
| macOS   | amd64 | `ccs-proxy-macos-amd64.tar.gz` |
| macOS   | arm64 | `ccs-proxy-macos-arm64.tar.gz` |
| Windows | amd64 | `ccs-proxy-windows-amd64.zip`  |
| Windows | arm64 | `ccs-proxy-windows-arm64.zip`  |

Linux/macOS 解压方式一样。按你下载的文件名执行即可。

Linux amd64 示例：

```bash
tar -xzf ccs-proxy-linux-amd64.tar.gz
chmod +x ccs-proxy
```

macOS arm64 示例：

```bash
tar -xzf ccs-proxy-macos-arm64.tar.gz
chmod +x ccs-proxy
```

其他 Linux/macOS 包只需要替换 `tar -xzf` 后面的文件名。

Windows 解压 `zip` 后直接使用里面的 `ccs-proxy.exe`。

解压后，把 `ccs-proxy` 或 `ccs-proxy.exe` 放到你习惯的位置即可。如果不想移动文件，也可以在解压目录里直接运行。

<details>
<summary>方式 B：自己编译</summary>

需要先安装 Rust。

开发构建：

```bash
cargo build
```

正式构建：

```bash
cargo build --release
```

编译后的文件在：

- macOS/Linux：`target/release/ccs-proxy`
- Windows：`target\release\ccs-proxy.exe`

也可以直接安装到本机 Cargo bin 目录：

```bash
cargo install --path .
```

</details>

### 步骤二：启动

直接启动会使用默认上游：

```bash
ccs-proxy
```

指定上游地址：

```bash
CCS_PROXY_UPSTREAM_BASE_URL=https://your-proxy.example ccs-proxy
```

Windows PowerShell：

```powershell
$env:CCS_PROXY_UPSTREAM_BASE_URL = "https://your-proxy.example"
.\ccs-proxy.exe
```

## 配置项

环境变量和命令行参数都支持。两者同时存在时，命令行参数优先。

| 环境变量                      | 命令行参数            | 默认值                         | 说明                     |
| ----------------------------- | --------------------- | ------------------------------ | ------------------------ |
| `CCS_PROXY_LISTEN`            | `--listen`            | `127.0.0.1:8000`               | 本地监听地址             |
| `CCS_PROXY_UPSTREAM_BASE_URL` | `--upstream-base-url` | `https://api.claudecode.store` | 上游服务地址             |
| `CCS_PROXY_UPSTREAM_PREFIX`   | `--upstream-prefix`   | 空                             | 转发前自动加上的路径前缀 |
| `RUST_LOG`                    | 无                    | `info`                         | 日志级别，常用值见下方   |

`RUST_LOG` 常用值：

| 值      | 说明                                     |
| ------- | ---------------------------------------- |
| `off`   | 关闭日志                                 |
| `error` | 只看错误                                 |
| `warn`  | 看警告和错误                             |
| `info`  | 默认值，适合日常使用                     |
| `debug` | 看更详细的转发信息，排查问题时使用       |
| `trace` | 最详细，日志很多，一般只在深入排查时使用 |

也可以只打开本项目的详细日志：

```bash
RUST_LOG=ccs_proxy=debug ccs-proxy
```

示例：

```bash
ccs-proxy \
  --listen 127.0.0.1:8000 \
  --upstream-base-url https://your-proxy.example \
  --upstream-prefix /your-route/backend-api
```

## 路径怎么转发

转发规则很直接：

```text
上游地址 + 可选前缀 + Codex App 请求的原始路径和查询参数
```

例如：

```text
https://localhost:8000/wham/remote/control/server
-> https://your-proxy.example/your-route/backend-api/wham/remote/control/server
```

`ccs-proxy` 不会猜测、改写业务路径。如果 Codex App 请求的是 `/backend-api/codex/beacons/home`，上游收到的也是这个路径。路径是否正确，应由你的上游服务或 Codex 配置决定。

## 自动 HTTPS / WSS（macOS / Linux / Windows）

直接运行：

```bash
ccs-proxy
```

默认监听 `https://localhost:8000`，无需额外代理服务或手动运行证书生成命令。启动流程：

1. 先检查监听端口是否可用；被占用则报错退出，不弹证书授权。
2. 在用户证书目录检查本地 CA、服务端证书和私钥，不存在则生成。macOS/Linux 使用 `$XDG_CONFIG_HOME/ccs-proxy/tls` 或 `~/.config/ccs-proxy/tls`；Windows 使用 `%LOCALAPPDATA%\ccs-proxy\tls`。
3. 检查证书签名、`localhost` 域名、有效期和私钥匹配；服务端证书临近过期（30 天内）时复用 CA 重新签发。CA 临近过期则更新，并重新检查系统信任。
4. 按当前系统检查证书信任。未信任时打印用途和证书位置，发起授权；桌面弹窗或终端提示因平台而异。
5. 授权后重新验证信任，成功才开始提供 HTTPS/WSS。取消或验证失败则停止启动，不会偷偷降级为 HTTP。

CA 有效期 10 年，服务端证书有效期 365 天。私钥受用户目录权限保护（Unix 文件权限 600；Windows 专用目录 ACL 仅允许当前用户完全控制）。后续启动会复用有效证书和已有信任，不重复请求授权。CA 文件不完整或私钥不匹配会明确报错，不静默覆盖已有 CA。

| 平台 | 信任与授权方式 |
| --- | --- |
| macOS | 使用 `security` 验证证书链，系统授权后安装到用户登录钥匙串。 |
| Windows | 用 Windows 证书链验证，确认弹窗后安装到 `CurrentUser\Root`，不修改全机证书库；使用系统 Windows PowerShell。 |
| Linux | OpenSSL 验证系统信任；优先用桌面 `pkexec` 授权，否则终端 `sudo`；root 可直接安装。 |

Linux 支持 `update-ca-certificates`（Debian/Ubuntu、Alpine、openSUSE）和 `update-ca-trust`（Fedora/RHEL、Arch）对应的信任目录，需安装 `openssl`、`ca-certificates`。若已有 Chromium/Electron NSS 数据库（`~/.pki/nssdb` 或 `~/.local/share/pki/nssdb`），还会检查并安装该库的 CA 信任，需提供 `certutil`（Debian/Ubuntu 的 `libnss3-tools`、Fedora/RHEL 的 `nss-tools`）。缺少工具会明确报错，不会跳过验证或擅自安装软件包。无桌面会话时使用终端授权；无交互服务首次运行应预先完成信任配置。

Linux 安装的是系统 CA，Windows/macOS 默认安装用户 CA；授权后都会再次验证。某些使用独立信任库的应用仍需单独配置，不能仅凭系统信任就保证所有应用均可用。

自动流程不修改 Codex 配置、账户凭据、环境变量或请求路径。

### 保留当前完整 URL 配置方式

**房间路径仍写在 App 的 URL 中，不需要迁移到 `--upstream-prefix`。只将原 URL 的 `http` 改为 `https`。** 例如你原来使用 `/agents/codex-room/ROOM_ID/backend-api/codex`：

```toml
chatgpt_base_url = "https://localhost:8000/agents/codex-room/ROOM_ID/backend-api/codex"

# 保留当前 model_providers 表，只修改原 base_url 的协议：
# base_url = "https://localhost:8000/agents/codex-room/ROOM_ID/backend-api/codex"
```

桌面环境变量同样保留完整路径：

```bash
launchctl setenv CODEX_API_BASE_URL "https://localhost:8000/agents/codex-room/ROOM_ID/backend-api/codex"
```

代理按收到的路径原样转发：

```text
https://localhost:8000/agents/codex-room/ROOM_ID/backend-api/codex/wham/accounts/check
→ https://api.claudecode.store/agents/codex-room/ROOM_ID/backend-api/codex/wham/accounts/check
```

TLS 就绪不代表 App 登录全部通过。新版 App 的工作区路由可能仅保留 origin 而丢弃完整路径；本代理不会自动猜测或补回房间路径。遇到此类请求应结合实际日志诊断。账户 ID 一致性及 Codex 子进程的 CA 信任也需要分别验证。

如果 Codex Rust 子进程没有读取系统信任，可以在启动 App 前显式配置 `SSL_CERT_FILE` 指向上述 `ca.pem`；程序不会自动设置该环境变量。

### 自行提供证书

高级用法仍支持 PEM 证书链和私钥（两个参数必须同时提供）。此模式不生成证书、不修改系统信任，证书信任由使用者管理：

```bash
ccs-proxy --tls-cert /path/to/cert.pem --tls-key /path/to/key.pem
```

| 参数 | 环境变量 | 用途 |
| --- | --- | --- |
| `--tls-cert` | `CCS_PROXY_TLS_CERT` | 服务端证书链 |
| `--tls-key` | `CCS_PROXY_TLS_KEY` | 服务端私钥 |

验证本地监听：

```bash
curl https://localhost:8000/healthz
```

`healthz` 只验证本地 HTTPS，完整登录仍需 App 的 `account/read` 和实际业务请求成功。

## 登录头补发

有些 Codex App 请求会带 `Authorization` 和 `ChatGPT-Account-Id`，有些页面探测请求可能不带。

`ccs-proxy` 会记住本进程内最近一次看到的非空登录头。后续请求缺少这些头时，它会自动补上再转发给上游。

这个能力只在当前进程内生效：

- 不读取 `~/.codex/auth.json`
- 不创建、不刷新、不解析 token
- 重启 `ccs-proxy` 后缓存会清空
- 如果看到新的 `Authorization`，但没有新的 `ChatGPT-Account-Id`，旧账号 ID 会被清掉，避免新 token 配旧账号

## 常见问题

### 为什么一定推荐 `localhost:8000`

因为 Codex App 对哪些本地地址自动附带登录头有限制。`localhost:8000` 是最稳妥的默认值。

### 这个工具会帮我登录 ChatGPT 吗

不会。它只转发请求，并在进程内补发已经见过的请求头。登录、账号、token 都由 Codex App 和上游服务处理。

### 上游需要做什么

上游必须自己支持 Codex/ChatGPT 需要的接口路径和行为。`ccs-proxy` 只是本地中转，不实现上游业务。
