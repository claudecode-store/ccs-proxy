# ccs-proxy

`ccs-proxy` 是一个本地转发工具。

简单说：Codex App 只能稳定地把登录信息带到 `http://localhost:8000`，但你真正要访问的服务可能在别的地址。这个工具就在中间转一下，把 Codex App 发到本机的 HTTP 和 WebSocket 请求，转发到你配置的上游服务。

## 什么时候需要它

你需要让 Codex App 访问一个自定义的 ChatGPT/Codex 上游服务时，可以用它。

推荐在 Codex App 里使用：

```text
http://localhost:8000
```

不要随便改成 `127.0.0.1` 或其他端口。部分 Codex App 版本只会给 `localhost:8000` 自动带上 ChatGPT 登录头，换掉后可能出现上游返回 `401`、`403` 或连接失败。

## 使用步骤

### 步骤一：下载或自己编译

推荐直接下载发布包。只有在你想自己改代码，或发布页没有适合你系统的包时，才需要自己编译。

#### 方式 A：下载发布包

每次发布版本都会在 GitHub Releases 里提供 6 个二进制包：

| 系统 | 架构 | 文件 |
| --- | --- | --- |
| Linux | amd64 | `ccs-proxy-linux-amd64.tar.gz` |
| Linux | arm64 | `ccs-proxy-linux-arm64.tar.gz` |
| macOS | amd64 | `ccs-proxy-macos-amd64.tar.gz` |
| macOS | arm64 | `ccs-proxy-macos-arm64.tar.gz` |
| Windows | amd64 | `ccs-proxy-windows-amd64.zip` |
| Windows | arm64 | `ccs-proxy-windows-arm64.zip` |

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

| 环境变量 | 命令行参数 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `CCS_PROXY_LISTEN` | `--listen` | `127.0.0.1:8000` | 本地监听地址 |
| `CCS_PROXY_UPSTREAM_BASE_URL` | `--upstream-base-url` | `https://chatgpt.claudecode.store` | 上游服务地址 |
| `CCS_PROXY_UPSTREAM_PREFIX` | `--upstream-prefix` | 空 | 转发前自动加上的路径前缀 |
| `RUST_LOG` | 无 | `info` | 日志级别，常用值见下方 |

`RUST_LOG` 常用值：

| 值 | 说明 |
| --- | --- |
| `off` | 关闭日志 |
| `error` | 只看错误 |
| `warn` | 看警告和错误 |
| `info` | 默认值，适合日常使用 |
| `debug` | 看更详细的转发信息，排查问题时使用 |
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
http://localhost:8000/wham/remote/control/server
-> https://your-proxy.example/your-route/backend-api/wham/remote/control/server
```

`ccs-proxy` 不会猜测、改写业务路径。如果 Codex App 请求的是 `/backend-api/codex/beacons/home`，上游收到的也是这个路径。路径是否正确，应由你的上游服务或 Codex 配置决定。

## Codex App 怎么配

推荐方式是把上游固定前缀放到 `CCS_PROXY_UPSTREAM_PREFIX`，然后 Codex App 只配本地地址。

启动代理：

```bash
CCS_PROXY_UPSTREAM_BASE_URL=https://your-proxy.example \
CCS_PROXY_UPSTREAM_PREFIX=/your-route/backend-api \
ccs-proxy
```

然后在 `~/.codex/config.toml` 中配置：

```toml
chatgpt_base_url = "http://localhost:8000"
```

如果你不想用 `CCS_PROXY_UPSTREAM_PREFIX`，也可以把完整路径写进 Codex 配置：

```toml
chatgpt_base_url = "http://localhost:8000/your-route/backend-api"
```

这两种方式二选一。不要两边都写同一段路径，否则路径会重复。

另外，不要把 `chatgpt_base_url` 配成以 `/backend-api/codex` 结尾。Codex App 会自己追加不同业务路径，例如：

- 模型请求：`/backend-api/codex/...`
- 远程控制：`/backend-api/wham/remote/control/...`
- 连接器：`/backend-api/aip/connectors/...`
- 心跳或页面探测：`/backend-api/beacons/...`

如果你提前写死到 `/backend-api/codex`，其他请求会被拼错路径。

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
